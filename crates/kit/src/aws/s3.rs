//! [`ObjectStore`] e [`UploadSigner`] sobre o S3.
//!
//! O envio direto usa `PutObject` pré-assinado por 5 minutos com
//! `Content-Type`, `Content-Length` e `x-amz-checksum-sha256` presos à
//! assinatura: o cliente só consegue gravar **aquele** arquivo, daquele
//! tamanho, naquela chave (S-14). O balde de quarentena não é público e tem
//! CORS só para `saci.art.br`.

use std::collections::HashMap;
use std::time::Duration;

use aws_sdk_s3::Client;
use aws_sdk_s3::presigning::PresigningConfig;
use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::types::ChecksumAlgorithm;

use crate::BoxFut;
use crate::objects::{Object, ObjectError, ObjectStore, SignedUpload, UploadGrant, UploadSigner};

/// Um balde.
#[derive(Debug, Clone)]
pub struct S3Objects {
    client: Client,
    bucket: String,
}

impl S3Objects {
    /// Cria o adaptador. No LocalStack, o endereço vem de `AWS_ENDPOINT_URL` (estilo de caminho).
    #[must_use]
    pub fn new(config: &aws_config::SdkConfig, bucket: impl Into<String>) -> Self {
        let local = std::env::var("AWS_ENDPOINT_URL").is_ok();
        let conf = aws_sdk_s3::config::Builder::from(config)
            .force_path_style(local)
            .build();
        Self {
            client: Client::from_conf(conf),
            bucket: bucket.into(),
        }
    }
}

fn err(e: impl std::fmt::Display) -> ObjectError {
    ObjectError(e.to_string())
}

impl ObjectStore for S3Objects {
    fn put<'a>(
        &'a self,
        key: &'a str,
        bytes: Vec<u8>,
        content_type: &'a str,
    ) -> BoxFut<'a, Result<(), ObjectError>> {
        Box::pin(async move {
            self.client
                .put_object()
                .bucket(&self.bucket)
                .key(key)
                .content_type(content_type)
                .body(ByteStream::from(bytes))
                .send()
                .await
                .map_err(|e| err(e.into_service_error()))?;
            Ok(())
        })
    }

    fn get<'a>(&'a self, key: &'a str) -> BoxFut<'a, Result<Option<Object>, ObjectError>> {
        Box::pin(async move {
            match self
                .client
                .get_object()
                .bucket(&self.bucket)
                .key(key)
                .send()
                .await
            {
                Ok(out) => {
                    let content_type = out
                        .content_type
                        .clone()
                        .unwrap_or_else(|| "application/octet-stream".into());
                    let bytes = out.body.collect().await.map_err(err)?.into_bytes().to_vec();
                    Ok(Some(Object {
                        bytes,
                        content_type,
                    }))
                }
                Err(e) => {
                    let e = e.into_service_error();
                    if e.is_no_such_key() {
                        Ok(None)
                    } else {
                        Err(err(e))
                    }
                }
            }
        })
    }

    fn delete<'a>(&'a self, key: &'a str) -> BoxFut<'a, Result<(), ObjectError>> {
        Box::pin(async move {
            self.client
                .delete_object()
                .bucket(&self.bucket)
                .key(key)
                .send()
                .await
                .map_err(|e| err(e.into_service_error()))?;
            Ok(())
        })
    }
}

impl UploadSigner for S3Objects {
    fn sign_upload<'a>(
        &'a self,
        grant: &'a UploadGrant,
    ) -> BoxFut<'a, Result<SignedUpload, ObjectError>> {
        Box::pin(async move {
            let config =
                PresigningConfig::expires_in(Duration::from_secs(grant.expires_in)).map_err(err)?;
            let request = self
                .client
                .put_object()
                .bucket(&self.bucket)
                .key(&grant.key)
                .content_type(&grant.content_type)
                .content_length(i64::try_from(grant.content_length).unwrap_or(i64::MAX))
                .checksum_algorithm(ChecksumAlgorithm::Sha256)
                .checksum_sha256(&grant.checksum_sha256)
                .presigned(config)
                .await
                .map_err(err)?;
            let headers: HashMap<String, String> = request
                .headers()
                .filter(|(k, _)| {
                    !k.eq_ignore_ascii_case("host") && !k.eq_ignore_ascii_case("content-length")
                })
                .map(|(k, v)| (k.to_owned(), v.to_owned()))
                .collect();
            Ok(SignedUpload {
                url: request.uri().to_owned(),
                headers,
            })
        })
    }
}
