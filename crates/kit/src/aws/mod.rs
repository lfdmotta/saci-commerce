//! Adaptadores AWS (feature `aws`). Nenhum serviço importa a AWS fora daqui e
//! do próprio módulo `aws` de cada serviço.
//!
//! | Módulo | Adaptador |
//! | --- | --- |
//! | [`dynamodb`] | [`crate::store::DocumentStore`] sobre uma tabela `pk`/`sk`, com *outbox* compatível com `saci-outbox` |
//! | [`lambda`] | Entradas de Lambda: HTTP do API Gateway, fila SQS de eventos, chamada interna, retransmissão do *outbox* |
//! | [`invoke`] | [`crate::internal::InternalClient`] por invocação direta de Lambda (IAM) |
//! | [`authz`] | Concessões de vitrine na tabela `authz` da plataforma |
//! | [`s3`] | Arquivos e envio direto pré-assinado |
//! | [`cloudfront`] | URLs assinadas de `files.` |
//! | [`kms`] | Assinatura de tokens com chave do KMS |
//! | [`optional_auth`] | Conta opcional em rota pública (mesmo código do autorizador) |
//! | [`ssm`] | Parâmetros e segredos do SSM lidos no início da função |

pub mod authz;
pub mod cloudfront;
pub mod dynamodb;
pub mod invoke;
pub mod kms;
pub mod lambda;
pub mod optional_auth;
pub mod s3;
pub mod ssm;

/// Configuração do SDK, uma vez por início de função. Localmente,
/// `AWS_ENDPOINT_URL` aponta para o LocalStack.
pub async fn sdk_config() -> aws_config::SdkConfig {
    aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await
}
