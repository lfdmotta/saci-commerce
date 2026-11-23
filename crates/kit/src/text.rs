//! Texto: busca sem acento, moderação de anúncios e máscaras.

/// Minúsculas e sem acento (a mesma regra de `utils/text.ts` da interface).
#[must_use]
pub fn normalize(text: &str) -> String {
    text.chars()
        .flat_map(char::to_lowercase)
        .map(|c| match c {
            'á' | 'à' | 'â' | 'ã' | 'ä' => 'a',
            'é' | 'ê' | 'è' | 'ë' => 'e',
            'í' | 'î' | 'ì' | 'ï' => 'i',
            'ó' | 'ô' | 'õ' | 'ò' | 'ö' => 'o',
            'ú' | 'û' | 'ù' | 'ü' => 'u',
            'ç' => 'c',
            'ñ' => 'n',
            other => other,
        })
        .collect()
}

/// As palavras de uma busca livre, sem acento nem caixa.
#[must_use]
pub fn words(q: Option<&str>) -> Vec<String> {
    q.map(|q| normalize(q).split_whitespace().map(str::to_owned).collect())
        .unwrap_or_default()
}

/// Todas as palavras aparecem no texto (já normalizado)?
#[must_use]
pub fn matches_all(haystack: &str, words: &[String]) -> bool {
    words.iter().all(|w| haystack.contains(w.as_str()))
}

/// Primeira letra maiúscula.
#[must_use]
pub fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    chars
        .next()
        .map(|c| c.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// Primeiro nome.
#[must_use]
pub fn first_name(name: &str) -> String {
    name.split_whitespace()
        .next()
        .unwrap_or_default()
        .to_owned()
}

/// Nome de URL a partir de um nome livre (`Ateliê Barro Vivo` → `atelie-barro-vivo`).
#[must_use]
pub fn slugify(text: &str) -> String {
    let mut out = String::new();
    for c in normalize(text).chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    out.trim_end_matches('-').to_owned()
}

/// Por que um texto de anúncio foi recusado.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Violation {
    /// E-mail, telefone ou rede social.
    Contact,
    /// Endereço de site.
    Link,
    /// Preço no texto (venda por fora).
    Price,
}

/// Moderação do nome e da descrição do anúncio (regra herdada do mock e S-07):
/// sem contato, link nem preço. Evita venda por fora e limita a injeção de
/// instruções na Caipora. É uma primeira barreira barata, não um classificador.
#[must_use]
pub fn listing_violation(text: &str) -> Option<Violation> {
    let lower = normalize(text);
    // E-mail ou @perfil.
    if lower.split_whitespace().any(|w| {
        let w = w.trim_matches(|c: char| !c.is_alphanumeric() && c != '@' && c != '.');
        (w.contains('@') && w.len() > 2) || w.starts_with('@')
    }) {
        return Some(Violation::Contact);
    }
    for marker in [
        "whatsapp",
        "whats ",
        "zap ",
        "zapzap",
        "instagram",
        "insta ",
        "telegram",
        "fale comigo",
        "me chama",
    ] {
        if lower.contains(marker) {
            return Some(Violation::Contact);
        }
    }
    for marker in [
        "http://", "https://", "www.", ".com", ".br", ".net", ".org", "bit.ly", "wa.me",
    ] {
        if lower.contains(marker) {
            return Some(Violation::Link);
        }
    }
    if lower.contains("r$") || lower.contains(" reais") {
        return Some(Violation::Price);
    }
    // Sequência de 8+ dígitos (com espaço, ponto, hífen ou parênteses): telefone.
    let mut digits = 0;
    for c in lower.chars() {
        if c.is_ascii_digit() {
            digits += 1;
            if digits >= 8 {
                return Some(Violation::Contact);
            }
        } else if !matches!(c, ' ' | '-' | '.' | '(' | ')' | '+') {
            digits = 0;
        }
    }
    None
}

/// CPF mascarado: `***.456.789-**`.
#[must_use]
pub fn mask_cpf(cpf: &str) -> String {
    let d: String = cpf.chars().filter(char::is_ascii_digit).collect();
    if d.len() != 11 {
        return "***.***.***-**".into();
    }
    format!("***.{}.{}-**", &d[3..6], &d[6..9])
}

/// CNPJ mascarado: `**.456.789/****-**`.
#[must_use]
pub fn mask_cnpj(cnpj: &str) -> String {
    let d: String = cnpj.chars().filter(char::is_ascii_digit).collect();
    if d.len() != 14 {
        return "**.***.***/****-**".into();
    }
    format!("**.{}.{}/****-**", &d[2..5], &d[5..8])
}

/// E-mail mascarado: `m***@gmail.com`.
#[must_use]
pub fn mask_email(email: &str) -> String {
    match email.split_once('@') {
        Some((user, domain)) => {
            format!("{}***@{domain}", user.chars().next().unwrap_or('*'))
        }
        None => "***".into(),
    }
}

/// Celular mascarado: `(85) *****-1234`, ou `+55 (85) *****-1234` em E.164.
#[must_use]
pub fn mask_phone(phone: &str) -> String {
    let d: String = phone.chars().filter(char::is_ascii_digit).collect();
    if phone.trim_start().starts_with('+') {
        // E.164: `+55 (85) *****-7777` no Brasil.
        if let Some(national) = d.strip_prefix("55").filter(|n| n.len() >= 10) {
            return format!(
                "+55 ({}) *****-{}",
                &national[..2],
                &national[national.len() - 4..]
            );
        }
        // Fora do Brasil, o DDI exato fica com o cofre (que tem a lista).
        if d.len() < 8 {
            return "+** *****-****".into();
        }
        return format!("+** *****{}", &d[d.len() - 4..]);
    }
    if d.len() < 10 {
        return "(**) *****-****".into();
    }
    format!("({}) *****-{}", &d[..2], &d[d.len() - 4..])
}

/// Celular informado: em E.164 (`+5585988887777`), ou o brasileiro antigo sem
/// DDI (10 ou 11 dígitos), que vira `+55…`. Só o formato: **quem confere o
/// DDI na lista aceita é o cofre** do `saci-identity` (`phone-countries.yaml`
/// do `saci-contracts`).
#[must_use]
pub fn normalize_phone(phone: &str) -> Option<String> {
    let t = phone.trim();
    if t.is_empty()
        || !t
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '+' | ' ' | '(' | ')' | '-' | '.'))
        || t.chars().skip(1).any(|c| c == '+')
    {
        return None;
    }
    let d: String = t.chars().filter(char::is_ascii_digit).collect();
    let full = if t.starts_with('+') {
        d
    } else if (10..=11).contains(&d.len()) {
        format!("55{d}")
    } else {
        return None;
    };
    ((8..=15).contains(&full.len()) && !full.starts_with('0')).then(|| format!("+{full}"))
}

/// Troca padrões de dado pessoal num texto livre (chamados, S-23).
#[must_use]
pub fn redact(text: &str) -> String {
    saci_mask::mask(text).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_and_slugifies() {
        assert_eq!(normalize("Cerâmica de Açúcar"), "ceramica de acucar");
        assert_eq!(slugify("Ateliê  Barro Vivo!"), "atelie-barro-vivo");
        assert_eq!(words(Some("  Barro VIVO ")), ["barro", "vivo"]);
    }

    #[test]
    fn moderation_catches_contact_link_and_price() {
        assert_eq!(
            listing_violation("Chama no whatsapp"),
            Some(Violation::Contact)
        );
        assert_eq!(
            listing_violation("ver em loja.com.br"),
            Some(Violation::Link)
        );
        assert_eq!(
            listing_violation("Por R$ 50 no pix"),
            Some(Violation::Price)
        );
        assert_eq!(
            listing_violation("Ligue (11) 98765-4321"),
            Some(Violation::Contact)
        );
        assert_eq!(
            listing_violation("me siga @ateliebarro"),
            Some(Violation::Contact)
        );
        assert_eq!(
            listing_violation("Moringa de barro com 32 cm, queimada a lenha"),
            None
        );
    }

    #[test]
    fn masks() {
        assert_eq!(mask_cpf("12345678900"), "***.456.789-**");
        assert_eq!(mask_cnpj("12345678000190"), "**.345.678/****-**");
        assert_eq!(mask_email("maria@gmail.com"), "m***@gmail.com");
        assert_eq!(mask_phone("85987651234"), "(85) *****-1234");
        assert_eq!(mask_phone("+5585987651234"), "+55 (85) *****-1234");
        assert_eq!(
            normalize_phone("85987651234").as_deref(),
            Some("+5585987651234")
        );
        assert_eq!(
            normalize_phone("+351 912 345 678").as_deref(),
            Some("+351912345678")
        );
        assert_eq!(normalize_phone("abc"), None);
    }
}
