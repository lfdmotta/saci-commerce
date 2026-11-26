//! Recusas no formato RFC 9457, com o catálogo de códigos de
//! `saci-contracts/common/error-codes.yaml`.
//!
//! O tipo é o [`Problem`] da plataforma (`saci-problem`). Este módulo só garante
//! que o `code`, o `status`, o `title` e a mensagem padrão sejam os do catálogo
//! dos contratos, que é o que a interface lê (`code`, `message`, `fields`).
//!
//! As bibliotecas da plataforma usam alguns nomes anteriores ao catálogo
//! (`unauthenticated`, `reauthentication-required`, `idempotency-key-reused`).
//! [`normalize`] os troca pelos nomes do contrato antes de a resposta sair.

use saci_problem::Problem;

/// Uma linha do catálogo: código, status, título e mensagem padrão.
type Entry = (&'static str, u16, &'static str, &'static str);

/// O catálogo de `saci-contracts/common/error-codes.yaml` usado neste repositório.
const CATALOG: &[Entry] = &[
    (
        "unauthorized",
        401,
        "Sessão necessária",
        "Entre na sua conta para continuar.",
    ),
    (
        "session-expired",
        401,
        "Sessão vencida",
        "Sessão expirada. Entre de novo.",
    ),
    (
        "reauth-required",
        401,
        "Confirme a senha",
        "Confirme sua senha para continuar.",
    ),
    (
        "forbidden",
        403,
        "Sem acesso",
        "Você não tem acesso a esta parte do painel.",
    ),
    (
        "not-found",
        404,
        "Não encontrado",
        "Não encontramos o que você procurou.",
    ),
    (
        "precondition-failed",
        412,
        "Alterado por outra pessoa",
        "Outra pessoa salvou antes. Abra de novo para ver a versão atual.",
    ),
    (
        "validation-failed",
        422,
        "Dados recusados",
        "Confira os campos destacados.",
    ),
    (
        "idempotency-mismatch",
        422,
        "Chave de idempotência já usada",
        "Esta chave já foi usada em outra operação.",
    ),
    (
        "rate-limited",
        429,
        "Muitas tentativas",
        "Muitas tentativas. Espere um pouco e tente de novo.",
    ),
    (
        "service-unavailable",
        503,
        "Serviço indisponível",
        "Não conseguimos concluir agora. Nada foi alterado; tente de novo em instantes.",
    ),
    (
        "invalid-credentials",
        401,
        "Entrada ou senha não conferem",
        "E-mail ou celular e senha não conferem.",
    ),
    (
        "wrong-password",
        401,
        "Senha não confere",
        "A senha não confere.",
    ),
    (
        "weak-password",
        422,
        "Senha fraca",
        "A senha não atende às exigências.",
    ),
    (
        "invalid-code",
        422,
        "Código não confere",
        "O código não confere. Confira a mensagem e digite de novo.",
    ),
    (
        "code-expired",
        410,
        "Código vencido",
        "Este código venceu. Peça um novo.",
    ),
    (
        "privacy-request-open",
        409,
        "Pedido em andamento",
        "Já existe um pedido igual em andamento.",
    ),
    (
        "attempts-exceeded",
        429,
        "Tentativas esgotadas",
        "Muitas tentativas. Peça um código novo ou tente mais tarde.",
    ),
    (
        "contact-required",
        422,
        "Contato obrigatório",
        "Mantenha um e-mail ou um celular para receber os avisos.",
    ),
    // compra: o contato confirmado antes do pagamento (#35)
    (
        "contact-verification-required",
        422,
        "Confirme o contato",
        "Confirme o seu e-mail ou celular com o código antes de pagar.",
    ),
    // cadastro sem CPF obrigatório (#27)
    (
        "phone-country-not-supported",
        422,
        "DDI não aceito",
        "Ainda não enviamos mensagens para o país deste celular.",
    ),
    (
        "contact-unavailable",
        409,
        "Contato indisponível",
        "Este contato não pode ser usado nesta conta.",
    ),
    (
        "contact-already-verified",
        409,
        "Contato já confirmado",
        "Este contato já está confirmado.",
    ),
    (
        "document-required",
        422,
        "CPF necessário",
        "Informe o seu CPF para continuar.",
    ),
    (
        "document-already-set",
        409,
        "CPF já informado",
        "A conta já tem um CPF. Para corrigir, fale com a gente.",
    ),
    (
        "document-unavailable",
        409,
        "CPF indisponível",
        "Este CPF não pode ser usado nesta conta. Se ele é seu, fale com a gente.",
    ),
    (
        "identity-mismatch",
        422,
        "Identidade não confere",
        "A data de nascimento não confere com o CPF na Receita Federal.",
    ),
    (
        "underage",
        422,
        "Menor de idade",
        "Para vender na Saci é preciso ter 18 anos ou mais.",
    ),
    (
        "cnpj-inactive",
        422,
        "CNPJ inativo",
        "Este CNPJ não está ativo na Receita Federal.",
    ),
    (
        "cnpj-not-partner",
        422,
        "CPF fora do quadro de sócios",
        "O seu CPF não consta entre os sócios deste CNPJ.",
    ),
    (
        "storefront-exists",
        409,
        "A conta já tem vitrine",
        "Esta conta já tem uma vitrine.",
    ),
    (
        "url-name-taken",
        409,
        "Endereço ocupado",
        "Este endereço já é de outra vitrine.",
    ),
    (
        "name-not-allowed",
        422,
        "Nome não permitido",
        "Escolha outro nome para a vitrine.",
    ),
    (
        "needs-cnpj",
        422,
        "Equipe só com CNPJ",
        "A equipe é para vitrines que vendem com CNPJ.",
    ),
    (
        "already-member",
        409,
        "CPF já está na equipe",
        "Este CPF já tem acesso à vitrine.",
    ),
    (
        "invites-exceeded",
        429,
        "Convites demais",
        "Foram muitos convites hoje. Tente de novo amanhã.",
    ),
    (
        "listing-rejected",
        422,
        "Anúncio recusado",
        "Nome e descrição não podem ter contato, link ou preço.",
    ),
    (
        "photo-not-ready",
        409,
        "Foto ainda em verificação",
        "Uma das fotos ainda está sendo verificada. Tente de novo em instantes.",
    ),
    (
        "listing-has-open-orders",
        409,
        "Anúncio com pedido em aberto",
        "Este anúncio tem pedido em aberto. Conclua o pedido antes de retirar a peça.",
    ),
    (
        "invalid-state",
        409,
        "Situação não permite a ação",
        "A situação atual não permite esta ação.",
    ),
    (
        "label-not-ready",
        409,
        "Etiqueta ainda não disponível",
        "A etiqueta sai depois de \"Peça pronta\".",
    ),
    (
        "purchase-required",
        403,
        "Só quem comprou avalia",
        "Só quem comprou pode avaliar.",
    ),
    (
        "already-reviewed",
        409,
        "Já avaliado",
        "Você já avaliou esta compra.",
    ),
    (
        "unsupported-media-type",
        415,
        "Tipo de imagem não aceito",
        "Use uma imagem JPEG, PNG ou WebP.",
    ),
    (
        "content-too-large",
        413,
        "Arquivo grande demais",
        "A imagem passa do tamanho aceito.",
    ),
    (
        "item-unavailable",
        409,
        "Peça indisponível",
        "Uma das peças saiu da feira.",
    ),
    (
        "price-changed",
        409,
        "Total mudou",
        "O total mudou desde que você abriu a tela.",
    ),
    (
        "card-declined",
        402,
        "Cartão recusado",
        "O banco recusou o cartão.",
    ),
    (
        "ticket-resolved",
        409,
        "Chamado encerrado",
        "Este chamado já foi encerrado.",
    ),
    // Fora do catálogo dos contratos: só o servidor (500). Mensagem fixa, sem detalhe.
    (
        "internal-error",
        500,
        "Erro interno",
        "Algo deu errado do nosso lado. Tente de novo em instantes.",
    ),
];

/// Recusa com o status, o título e a mensagem padrão do catálogo.
///
/// Código fora do catálogo vira `internal-error` (e um aviso no log): um
/// código novo entra primeiro em `saci-contracts`.
#[must_use]
pub fn problem(code: &str) -> Problem {
    if let Some((code, status, title, message)) = CATALOG.iter().find(|e| e.0 == code) {
        return Problem::new(*status, code, title).with_message(*message);
    }
    tracing::warn!(code, "código de recusa fora do catálogo");
    problem("internal-error")
}

/// `validation-failed` com a mensagem de um campo (nome em `snake_case`).
#[must_use]
pub fn invalid(field: &str, message: &str) -> Problem {
    problem("validation-failed").with_field(field, message)
}

/// `not-found`: também para objeto de outra pessoa (não revela que existe).
#[must_use]
pub fn not_found() -> Problem {
    problem("not-found")
}

/// `service-unavailable`: dependência fora do ar. A causa vai para o log.
#[must_use]
pub fn unavailable(cause: &dyn std::fmt::Display) -> Problem {
    tracing::error!(error = %cause, "dependência indisponível");
    problem("service-unavailable")
}

/// `internal-error`: falha inesperada. A causa vai para o log, nunca para a resposta.
#[must_use]
pub fn internal(cause: &dyn std::fmt::Display) -> Problem {
    tracing::error!(error = %cause, "falha inesperada");
    problem("internal-error")
}

/// Troca os códigos das bibliotecas da plataforma pelos do catálogo dos
/// contratos, preservando `fields` e `request_id`.
#[must_use]
pub fn normalize(p: Problem) -> Problem {
    let renamed = match p.code.as_str() {
        "unauthenticated" => Some("unauthorized"),
        "reauthentication-required" => Some("reauth-required"),
        "idempotency-key-reused" => Some("idempotency-mismatch"),
        "idempotency-key-required" | "bad-request" => Some("validation-failed"),
        // A plataforma usa 400 para validação; o contrato usa 422.
        "validation-failed" if p.status == 400 => Some("validation-failed"),
        _ => None,
    };
    let Some(code) = renamed else {
        return p;
    };
    let mut out = problem(code);
    if p.message.is_some() && code == "validation-failed" {
        out.message.clone_from(&p.message);
    }
    out.fields = p.fields;
    out.request_id = p.request_id;
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_codes_have_contract_status() {
        assert_eq!(problem("card-declined").status, 402);
        assert_eq!(problem("photo-not-ready").status, 409);
        assert_eq!(problem("validation-failed").status, 422);
        assert_eq!(problem("desconhecido").code, "internal-error");
    }

    #[test]
    fn normalizes_platform_codes() {
        let p = normalize(Problem::unauthenticated());
        assert_eq!((p.status, p.code.as_str()), (401, "unauthorized"));
        let p = normalize(Problem::reauthentication_required());
        assert_eq!(p.code, "reauth-required");
        let p = normalize(Problem::validation().with_field("cep", "CEP inválido"));
        assert_eq!(p.status, 422);
        assert_eq!(p.fields["cep"], "CEP inválido");
    }
}
