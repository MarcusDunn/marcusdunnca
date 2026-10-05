//! Signed HTTP to Claude's Messages API on Amazon Bedrock.
//!
//! # Which of Bedrock's several endpoints this is, and why
//!
//! Bedrock exposes Claude through more than one endpoint, and they do not serve
//! the same models in the same places. This targets **`bedrock-runtime`**, at
//! `/anthropic/v1/messages` — the native Messages API shape, on the endpoint AWS
//! recommends for new applications.
//!
//! The alternative, `bedrock-mantle.{region}.api.aws`, was tried first and is
//! wrong for this app: it serves Claude Sonnet 5.5 in **`us-gov-west-1` only**.
//! Every call from a commercial region returns 404 `not_found_error` — not a
//! permissions failure, the model genuinely is not there. That is worth
//! recording because the model *is* listed by `ListFoundationModels` in
//! commercial regions and *does* have a marketplace agreement there, so the
//! control plane and the data plane disagree and only the data plane is honest.
//!
//! This also replaced `aws_sdk_bedrockruntime`'s Converse API. Converse still
//! serves this model, but the native shape is what Anthropic documents, and
//! Converse forces citations on to read a PDF visually — which matters here,
//! because `figure_recall` questions are about what a page looks like.
//!
//! There is no generated AWS SDK crate for this path and no Anthropic SDK for
//! Rust, so the request is built as JSON, signed with SigV4, and posted
//! directly. What is lost relative to Converse is local, typed validation of the
//! request shape; a malformed body is a 400 from the service, so the shapes in
//! [`crate::bedrock`] are the only guard.
//!
//! # Why the model is named by inference profile
//!
//! **In-region inference is not available for this model in any region.** The
//! bare `anthropic.claude-sonnet-5-5` is rejected; a geographic or global
//! profile prefix is required, and the prefix is what decides data residency:
//!
//!   * `us.` — keeps data within US *and Canada* regions.
//!   * `eu.` — keeps data within EU regions.
//!   * `global.` — routes anywhere, no residency constraint.
//!
//! So residency is a property of the model ID here, not of the endpoint region.
//! A cross-region profile is also why the permissions boundary needs its Bedrock
//! exemption: each routing target is authorized with `aws:RequestedRegion` set
//! to *that target's* region rather than the caller's. See
//! `global_service_actions` in bootstrap/iam.tf.

use aws_credential_types::provider::ProvideCredentials;
use aws_credential_types::provider::SharedCredentialsProvider;
use aws_sigv4::http_request::{sign, SignableBody, SignableRequest, SigningSettings};
use aws_sigv4::sign::v4;
use std::time::SystemTime;
use trainer_core::error::{aws, Error, Result};

/// SigV4 service name. `bedrock`, not `bedrock-runtime` and not
/// `bedrock-mantle`: the signing name and the hostname differ here, and signing
/// against the hostname produces a signature mismatch rather than anything that
/// names the real problem.
const SERVICE: &str = "bedrock";

/// The API version header the Messages API requires. Unrelated to the model, and
/// unchanged since 2023 — pinned rather than configurable because a different
/// value changes response shapes this crate parses.
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Holds everything reused across invocations: the TLS pool and the credential
/// provider's cache. Built once at cold start and leaked, like the AWS clients
/// it sits beside.
pub struct Client {
    http: reqwest::Client,
    credentials: SharedCredentialsProvider,
    region: String,
    url: String,
}

impl Client {
    /// `region` is the endpoint the request is sent to. It does not by itself
    /// decide where inference runs or where the data may travel — the model's
    /// profile prefix does that. See the module header.
    pub fn new(sdk: &aws_config::SdkConfig, region: String) -> Result<Self> {
        let credentials = sdk
            .credentials_provider()
            .ok_or_else(|| Error::Config("no AWS credentials provider available".into()))?;

        let http = reqwest::Client::builder()
            // A generation is a long call: a hundred-page PDF with thinking on
            // runs well past any default. Lambda's own timeout is the real
            // ceiling; this exists so a hung socket surfaces as an error rather
            // than consuming the whole invocation.
            .timeout(std::time::Duration::from_secs(600))
            .build()
            .map_err(aws)?;

        Ok(Self {
            http,
            credentials,
            url: format!("https://bedrock-runtime.{region}.amazonaws.com/anthropic/v1/messages"),
            region,
        })
    }

    /// POST one Messages API request and hand back the parsed response body.
    ///
    /// Errors carry the service's message. The Messages API returns failures as
    /// a JSON envelope with an `error.message`, which is far more useful than
    /// the status line — a rejected `thinking` field, an unentitled model and a
    /// model that does not exist on this endpoint arrive as 400, 403 and 404
    /// whose bodies are the only thing that distinguishes them.
    pub async fn messages(&self, body: &serde_json::Value) -> Result<serde_json::Value> {
        let bytes = serde_json::to_vec(body)?;

        let credentials = self.credentials.provide_credentials().await.map_err(aws)?;
        let identity = credentials.into();

        let params = v4::SigningParams::builder()
            .identity(&identity)
            .region(&self.region)
            .name(SERVICE)
            .time(SystemTime::now())
            .settings(SigningSettings::default())
            .build()
            .map_err(|e| Error::Aws(format!("building signing params: {e}")))?
            .into();

        // Every header signed here must also be sent, byte for byte, or the
        // signature does not verify. That is why they are listed once and reused
        // for both.
        let headers = [
            ("content-type", "application/json"),
            ("anthropic-version", ANTHROPIC_VERSION),
        ];

        let signable = SignableRequest::new(
            "POST",
            &self.url,
            headers.iter().copied(),
            SignableBody::Bytes(&bytes),
        )
        .map_err(|e| Error::Aws(format!("building signable request: {e}")))?;

        let (instructions, _signature) = sign(signable, &params)
            .map_err(|e| Error::Aws(format!("signing request: {e}")))?
            .into_parts();

        let mut request = self.http.post(&self.url);
        for (name, value) in headers {
            request = request.header(name, value);
        }
        // Authorization, x-amz-date, and x-amz-security-token for temporary
        // credentials. Added by the signer rather than by hand precisely so the
        // session-token case cannot be forgotten.
        for header in instructions.headers() {
            request = request.header(header.0, header.1);
        }

        let response = request.body(bytes).send().await.map_err(aws)?;
        let status = response.status();
        let text = response.text().await.map_err(aws)?;

        if !status.is_success() {
            let detail = serde_json::from_str::<serde_json::Value>(&text)
                .ok()
                .and_then(|v| {
                    v.get("error")
                        .and_then(|e| e.get("message"))
                        .and_then(|m| m.as_str())
                        .map(str::to_string)
                })
                .unwrap_or_else(|| text.chars().take(200).collect());

            // 400 is the model rejecting the request and will reject it again on
            // retry; 403 is model access or IAM; 404 is a model this endpoint
            // does not serve. None is worth a Lambda retry, but all are
            // infrastructure rather than the document's fault, so they stay
            // `Aws` and surface in CloudWatch.
            return Err(Error::Aws(format!("bedrock {status}: {detail}")));
        }

        Ok(serde_json::from_str(&text)?)
    }
}
