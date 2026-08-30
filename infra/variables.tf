variable "project" {
  description = "Short project slug, used as a prefix for resource names and tags."
  type        = string
  default     = "marcusdunnca"
}

variable "aws_region" {
  description = "Primary AWS region."
  type        = string
  default     = "ca-central-1"
}


variable "app_domain" {
  description = <<-EOT
    Fully-qualified domain the application is served from.

    marcusdunn.ca is registered at Cloudflare and stays there — no Route53
    delegation. A hosted zone would cost $0.50/month to automate two DNS records
    that are created once and then left alone.
  EOT
  type        = string
  default     = "study.aws.marcusdunn.ca"
}

variable "webauthn_rp_id" {
  description = <<-EOT
    WebAuthn Relying Party ID. Must be a registrable suffix of app_domain.

    Deliberately the apex, not the app subdomain: an RP ID cannot be widened
    later without re-registering every passkey, so starting at the apex keeps
    future subdomains usable with the same credentials.
  EOT
  type        = string
  default     = "marcusdunn.ca"
}

variable "dynamodb_read_capacity" {
  description = "Provisioned RCU. The perpetual free tier covers 25; on-demand mode is NOT covered and bills from the first request."
  type        = number
  default     = 5
}

variable "dynamodb_write_capacity" {
  description = "Provisioned WCU. See dynamodb_read_capacity."
  type        = number
  default     = 5
}

variable "webauthn_credentials" {
  description = <<-EOT
    Registered passkeys, as a JSON array of {id, public_key} objects.

    This is public data by construction — a WebAuthn credential ID and its
    public key are what the browser hands any relying party during a ceremony,
    and neither can be used to forge an assertion. It is a plain environment
    variable rather than an SSM parameter for that reason: nothing here is worth
    the KMS call on every cold start.

    Empty by default. There is no self-service registration endpoint; enrolling
    a passkey means pasting its record here and applying, which is the intended
    friction for a single-user app.
  EOT
  type        = string
  default     = "[]"
}

variable "bedrock_model_id" {
  description = <<-EOT
    Model the generate function invokes.

    **This is a `us.` inference profile, so document text leaves Canada.**
    That was a deliberate trade, not an oversight. Nova Lite carried a genuine
    in-region (`ca.`) profile and was chosen for it, but measured on a real
    document it produced questions answerable without reading the document,
    exposes no reasoning mode at any price, and still emitted malformed
    questions under a JSON Schema. Sonnet with a thinking budget did not.

    Reverting to in-region residency means setting this back to
    "ca.amazon.nova-lite-v1:0" and accepting those question-quality losses;
    nothing else in the stack depends on the choice.

    **Any cross-region profile depends on the Bedrock exemption in
    bootstrap/iam.tf's global_service_actions.** A cross-region inference
    profile authorizes each of its routing targets with `aws:RequestedRegion`
    set to *that target's* region, not the caller's: `us.` routes through
    us-east-1, us-east-2, ca-central-1 and us-west-2, so a call made to the
    ca-central-1 endpoint is authorized four times, twice against regions the
    region lock does not allow. Every Sonnet profile does this, and a `global.`
    profile additionally routes to a region-less ARN authorized with no
    `aws:RequestedRegion` at all.

    Without that exemption every generation fails with AccessDenied naming an
    explicit deny in the permissions boundary — which is what happened, twice,
    before it was added.

    Cost is roughly 7c per document against Nova's 0.1c — bounded by
    daily_document_cap and by the budget's spend brake, both unchanged.
  EOT
  type        = string
  default     = "us.anthropic.claude-sonnet-4-6"
}

variable "bedrock_thinking_budget_tokens" {
  description = <<-EOT
    Tokens the model may spend reasoning before it answers. Zero disables it.

    This is the lever that moves questions from "a well-read person could
    answer this" to "you had to have opened the document", which is the only
    property that makes the quiz worth taking. It bills as output tokens.

    Anthropic models only. The Nova family rejects the request field outright,
    so setting this while pointing bedrock_model_id at Nova fails every
    generation — the handler sends the field whenever this is non-zero.
  EOT
  type        = number
  default     = 3000

  validation {
    # Below about a thousand the model cannot finish a thought and the budget is
    # spent for nothing; the upper bound is a cost guard, since these are output
    # tokens at Sonnet's rate.
    condition     = var.bedrock_thinking_budget_tokens == 0 || (var.bedrock_thinking_budget_tokens >= 1024 && var.bedrock_thinking_budget_tokens <= 16000)
    error_message = "Thinking budget must be 0 (disabled) or between 1024 and 16000 tokens."
  }
}

variable "generate_retry_attempts" {
  description = <<-EOT
    Asynchronous retries Lambda makes for a failed generate invocation. Total
    attempts is this plus one.

    Two things read it, and they must agree: Lambda's own retry configuration,
    and the handler's MAX_GENERATION_ATTEMPTS. The handler treats an
    infrastructure failure as retryable — document back to `pending`, invocation
    failed, Lambda tries again — which is correct until the attempt that has no
    successor. On that one it must write `failed` instead, because no further S3
    event will ever be delivered for an object that already exists and a
    `pending` document offers the reader no Retry button.

    Both are derived from this variable in lambda.tf so the two cannot drift. If
    they ever do, documents strand silently.

    Each retry is another Bedrock call, so this is also a cost input.
  EOT
  type        = number
  default     = 1

  validation {
    condition     = var.generate_retry_attempts >= 0 && var.generate_retry_attempts <= 2
    error_message = "Lambda accepts 0, 1 or 2 asynchronous retry attempts."
  }
}

variable "max_pages" {
  description = <<-EOT
    Pages of a document the generate function will read before giving up.

    Bedrock bills per input token and a PDF page is on the order of a thousand
    of them, so this is the per-document cost ceiling. It is enforced in the
    handler because IAM has no condition key for token count.
  EOT
  type        = number
  default     = 100
}

variable "daily_document_cap" {
  description = <<-EOT
    Documents the generate function will process in a rolling day.

    The second half of the cost ceiling: max_pages bounds one invocation, this
    bounds how many invocations a runaway upload loop can produce. Counted in
    DynamoDB by the handler — S3 notifications have no throttle of their own,
    and the account budget alarm fires hours after the money is gone.
  EOT
  type        = number
  default     = 20
}

variable "max_upload_bytes" {
  description = <<-EOT
    Largest PDF the presigned PUT will accept.

    Bound by Bedrock, not by S3. The Converse document block caps around 4.5 MB,
    so a larger file uploads perfectly and then fails generation — the worst
    shape of failure, because the cost is paid and the feedback arrives a minute
    later on a different screen. Rejecting at the create call fails it in the
    place the user is looking.
  EOT
  type        = number
  default     = 4500000
}


variable "registration_token" {
  description = <<-EOT
    Shared secret gating the one-shot passkey enrolment routes. Empty disables
    them entirely.

    THIS IS SECRET AND MUST NOT BE COMMITTED. Note .gitignore ignores *.tfvars
    but deliberately un-ignores *.auto.tfvars — so this must not go in an
    auto.tfvars file.

    Deliberately not wired to CI. Apply it locally for the ceremony
    (`tofu apply -var=registration_token=...`), and the next CI apply reverts it
    to empty, which closes the enrolment window automatically rather than
    depending on anyone remembering.

    Minimum 32 characters, enforced at cold start. Generate with
    `openssl rand -base64 24`. Between enabling this and pasting credentials,
    enrolment is protected only by this secret on an unauthenticated endpoint
    whose sole throttle is the account concurrency limit of 10 — keep the window
    to minutes.
  EOT
  type        = string
  sensitive   = true
  default     = ""
}

variable "api_log_level" {
  description = <<-EOT
    Tracing level for the api handler.

    "debug" surfaces the reason an assertion was refused — which check failed,
    never key material. Normal operation is "info"; a login that fails with a
    bare "unauthorized" is the case to raise it for.
  EOT
  type        = string
  default     = "info"
}
