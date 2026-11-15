//! `media-pipeline`: a esteira das imagens enviadas (proposta 5.5, S-14).
//!
//! ```text
//! quarentena → (GuardDuty Malware Protection, na nuvem) → confere tamanho e SHA-256
//!   → decodifica (JPEG, PNG ou WebP; o resto é `not-an-image`) → tamanho mínimo
//!   → reescreve a imagem do zero (some o EXIF, inclusive o GPS do ateliê)
//!   → gera os tamanhos em paralelo → publica no CDN → `media.upload.ready.v1`
//! ```
//!
//! A reescrita é o controle principal: nada do arquivo original (metadados,
//! perfis, blocos extras, *payload* escondido) chega ao CDN, só os pixels.
//! Fotos e capas saem em JPEG; logotipos em PNG (transparência).

use std::io::Cursor;
use std::sync::Arc;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use image::{DynamicImage, ImageFormat, ImageReader};
use saci_commerce_kit::Problem;
use saci_commerce_kit::objects::ObjectStore;
use saci_media_api::{Media, UploadRecord};
use sha2::{Digest, Sha256};

/// Lados (em pixels) gerados para cada finalidade; o primeiro é o publicado em `public_url`.
#[must_use]
pub fn sizes(purpose: &str) -> &'static [u32] {
    match purpose {
        "logo" => &[512, 256],
        "hero" => &[2000, 1000],
        _ => &[1600, 800, 400],
    }
}

/// Menor lado aceito.
#[must_use]
pub fn min_side(purpose: &str) -> u32 {
    match purpose {
        "logo" => 128,
        "hero" => 1000,
        _ => 600,
    }
}

/// Dependências da esteira.
#[derive(Debug, Clone)]
pub struct Pipeline {
    /// O serviço de mídia (estado dos envios).
    pub media: Media,
    /// Balde da quarentena.
    pub quarantine: Arc<dyn ObjectStore>,
    /// Balde do CDN (`cdn.`).
    pub cdn: Arc<dyn ObjectStore>,
    /// Base pública do CDN (`https://cdn.saci.art.br`).
    pub cdn_base_url: String,
}

/// Uma variante pronta.
struct Variant {
    side: u32,
    bytes: Vec<u8>,
    width: u32,
    height: u32,
}

/// Resultado da verificação.
#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Publicada: URL, largura, altura.
    Ready(String, u32, u32),
    /// Recusada, com o motivo do contrato.
    Rejected(&'static str),
}

impl Pipeline {
    /// Processa o objeto `key` da quarentena. `malware` vem do GuardDuty na
    /// nuvem (`None` quando não há verificação, como no ambiente local).
    pub async fn process(
        &self,
        key: &str,
        malware: Option<bool>,
    ) -> Result<Option<Verdict>, Problem> {
        let Some(record) = self.media.by_key(key).await? else {
            tracing::warn!("objeto na quarentena sem envio registrado");
            self.quarantine.delete(key).await?;
            return Ok(None);
        };
        if record.status == "ready" || record.status == "rejected" {
            return Ok(None);
        }
        self.media.mark_scanning(&record).await?;
        let verdict = if malware == Some(true) {
            Verdict::Rejected("malware")
        } else {
            match self.quarantine.get(key).await? {
                None => Verdict::Rejected("not-an-image"),
                Some(object) => self.publish(&record, object.bytes).await?,
            }
        };
        let mut done = record;
        match &verdict {
            Verdict::Ready(url, w, h) => {
                done.status = "ready".into();
                done.public_url = Some(url.clone());
                done.width = Some(*w);
                done.height = Some(*h);
            }
            Verdict::Rejected(reason) => {
                done.status = "rejected".into();
                done.rejection_reason = Some((*reason).to_owned());
            }
        }
        self.media.finish(done).await?;
        self.quarantine.delete(key).await?;
        Ok(Some(verdict))
    }

    async fn publish(&self, record: &UploadRecord, bytes: Vec<u8>) -> Result<Verdict, Problem> {
        // O tamanho e o hash assinados no envio precisam conferir.
        let digest = STANDARD.encode(Sha256::digest(&bytes));
        if u64::try_from(bytes.len()).unwrap_or(0) != record.content_length
            || digest != record.checksum_sha256
        {
            return Ok(Verdict::Rejected("not-an-image"));
        }
        let purpose = record.purpose.clone();
        // Decodificar e reescrever é CPU: fora do executor assíncrono.
        let rendered = tokio_blocking(move || render(&bytes, &purpose))?;
        let variants = match rendered {
            Ok(v) => v,
            Err(reason) => return Ok(Verdict::Rejected(reason)),
        };
        let (ext, content_type) = if record.purpose == "logo" {
            ("png", "image/png")
        } else {
            ("jpg", "image/jpeg")
        };
        let base = format!("media/{}/{}", record.storefront_id, record.upload_id);
        let mut first = None;
        for v in variants {
            let key = format!("{base}/{}.{ext}", v.side);
            self.cdn.put(&key, v.bytes, content_type).await?;
            first.get_or_insert((key, v.width, v.height));
        }
        let (key, w, h) =
            first.ok_or_else(|| saci_commerce_kit::problem::internal(&"nenhuma variante"))?;
        Ok(Verdict::Ready(
            format!("{}/{key}", self.cdn_base_url.trim_end_matches('/')),
            w,
            h,
        ))
    }
}

/// Roda trabalho de CPU numa thread à parte e espera.
fn tokio_blocking<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, Problem> {
    std::thread::spawn(work)
        .join()
        .map_err(|_| saci_commerce_kit::problem::internal(&"falha ao processar a imagem"))
}

/// Decodifica, confere e reescreve em todos os tamanhos, em paralelo.
fn render(bytes: &[u8], purpose: &str) -> Result<Vec<Variant>, &'static str> {
    let reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|_| "not-an-image")?;
    if !matches!(
        reader.format(),
        Some(ImageFormat::Jpeg | ImageFormat::Png | ImageFormat::WebP)
    ) {
        return Err("not-an-image");
    }
    let image = reader.decode().map_err(|_| "not-an-image")?;
    if image.width().min(image.height()) < min_side(purpose) {
        return Err("too-small");
    }
    let image = Arc::new(image);
    let logo = purpose == "logo";
    let results: Vec<Result<Variant, &'static str>> = std::thread::scope(|scope| {
        let handles: Vec<_> = sizes(purpose)
            .iter()
            .map(|&side| {
                let image = Arc::clone(&image);
                scope.spawn(move || encode(&image, side, logo))
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().unwrap_or(Err("not-an-image")))
            .collect()
    });
    results.into_iter().collect()
}

fn encode(image: &DynamicImage, side: u32, logo: bool) -> Result<Variant, &'static str> {
    // Nunca aumenta: lado maior até `side`.
    let resized = if image.width().max(image.height()) > side {
        image.resize(side, side, FilterType::Lanczos3)
    } else {
        image.clone()
    };
    let mut out = Vec::new();
    if logo {
        resized
            .write_to(&mut Cursor::new(&mut out), ImageFormat::Png)
            .map_err(|_| "not-an-image")?;
    } else {
        let rgb = resized.to_rgb8();
        JpegEncoder::new_with_quality(&mut out, 82)
            .encode_image(&rgb)
            .map_err(|_| "not-an-image")?;
    }
    Ok(Variant {
        side,
        bytes: out,
        width: resized.width(),
        height: resized.height(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(w: u32, h: u32) -> Vec<u8> {
        let img = DynamicImage::new_rgb8(w, h);
        let mut out = Vec::new();
        img.write_to(&mut Cursor::new(&mut out), ImageFormat::Png)
            .unwrap_or_default();
        out
    }

    #[test]
    fn renders_sizes_and_rejects() {
        let variants = render(&png(1200, 900), "listing-photo").unwrap_or_default();
        assert_eq!(variants.len(), 3);
        assert_eq!((variants[0].width, variants[0].height), (1200, 900));
        assert_eq!(variants[1].width, 800);
        assert!(
            variants[0].bytes.starts_with(&[0xFF, 0xD8]),
            "JPEG reescrito"
        );
        assert_eq!(
            render(&png(300, 300), "listing-photo").err(),
            Some("too-small")
        );
        assert_eq!(render(b"<svg/>", "logo").err(), Some("not-an-image"));
    }
}
