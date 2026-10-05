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
    Model the generate function runs inference on, as a geographic inference
    profile.

    **The prefix is required, and it is the data-residency control.** In-region
    inference is unavailable for this model in every region, so a bare
    `anthropic.claude-sonnet-5-5` is rejected outright. The prefix decides where
    the document may travel:

      * `us.` — stays within US *and Canada* regions. What this uses.
      * `eu.` — stays within EU regions.
      * `global.` — routes anywhere, no residency constraint.

    So residency lives here rather than in bedrock_region, which is only the
    address the request is sent to. `us.` is the closest this model gets to
    staying in Canada — there is no `ca.` profile for it — and it is the same
    bargain the Sonnet 4.6 setup struck.

    **Sonnet 5 rather than 5.5, and not by preference.** 5.5 is gated by AWS
    account criteria that no API exposes: with the marketplace agreement accepted
    and get-foundation-model-availability reporting AVAILABLE / AUTHORIZED /
    AVAILABLE / AVAILABLE in every region the profile routes through — the same
    four values the working model reports — inference still returns 403 "not
    available for this account", closing with an invitation to contact AWS Sales.
    Sonnet 5 is documented as open to all Bedrock customers and carries the same
    $2/$10 per MTok on Bedrock. Moving to 5.5 is this value and nothing else; its
    agreement is already in place.

    **A denial here is invisible to CloudTrail.** Model invocation is a Bedrock
    data event and the trail carries management events, so the usual witness has
    nothing to say. Read get-foundation-model-availability first.

    Lineage, since it is the third model here: Nova Lite was first and was the
    only one with a genuine in-region (`ca.`) profile, dropped because on a real
    document it produced questions answerable without reading it, exposes no
    reasoning mode at any price, and emitted malformed questions even under a
    JSON Schema. Sonnet 4.6 with a thinking budget replaced it, and this
    replaced that — though 4.6 cannot be reverted to without also reverting the
    transport, since the native Messages API serves Sonnet 5 and later only and
    returns 404 for 4.6.
  EOT
  type        = string
  default     = "us.anthropic.claude-sonnet-5"
}

variable "bedrock_region" {
  description = <<-EOT
    Region whose bedrock-runtime endpoint receives the request.

    Only the address. Where inference runs, and where the document may travel, is
    decided by the profile prefix on bedrock_model_id — so this matching
    aws_region is for latency and for keeping the call inside the region lock,
    not for residency.

    This was briefly thought impossible. The `bedrock-mantle` endpoint has no
    Canadian presence and serves this model in us-gov-west-1 alone, which would
    have put the caller itself outside the region lock. `bedrock-runtime` is in
    ca-central-1 and reaches the model from there through a geographic profile,
    so only the profile's routing targets leave the allowed regions — which is
    what the Bedrock exemption in bootstrap/iam.tf has always covered.
  EOT
  type        = string
  default     = "ca-central-1"
}

variable "bedrock_effort" {
  description = <<-EOT
    How hard the model may think before answering.

    This is the lever that moves questions from "a well-read person could
    answer this" to "you had to have opened the document", which is the only
    property that makes the quiz worth taking. Thinking bills as output tokens.

    It replaced a 3000-token thinking budget, which this model rejects with a
    400. The two do not convert — a budget bought tokens, a level buys a
    disposition — so `high` here is the model's own default and a starting
    point, not a measured finding. Settling it means running the reference
    document at two or three levels and comparing the questions.
  EOT
  type        = string
  default     = "high"

  validation {
    condition     = contains(["low", "medium", "high", "xhigh", "max"], var.bedrock_effort)
    error_message = "Effort must be one of low, medium, high, xhigh, max."
  }
}

variable "bedrock_repair_attempts" {
  description = <<-EOT
    Times a rejected quiz is handed back to the model with the reason attached.

    The JSON Schema the model is given is advisory, not enforced: structured
    outputs are unavailable on Bedrock, so a tool call that violates the schema
    arrives anyway and the handler's own validation is what catches it. Until
    this existed, that was the end of the document — one malformed array and the
    upload was marked failed, with a Retry button that started over from the PDF
    and paid for the whole generation again.

    A repair turn is cheaper than that retry: the conversation already holds the
    document, so the correction is a short follow-up rather than a fresh upload.
    It is still a billed call, and generate_retry_attempts stacks on top, so the
    worst case per document is (repair_attempts + 1) x (retry_attempts + 1)
    model calls.

    One, because the observed failures are formatting slips an immediate
    correction fixes — an array sent as a string containing JSON — and a model
    that gets the shape wrong twice running is usually wrong about the document
    rather than the format. Zero restores the previous single-shot behaviour.
  EOT
  type        = number
  default     = 1

  validation {
    condition     = var.bedrock_repair_attempts >= 0 && var.bedrock_repair_attempts <= 3
    error_message = "Repair attempts must be between 0 and 3."
  }
}

variable "bedrock_max_output_tokens" {
  description = <<-EOT
    Ceiling on thinking plus answer, in tokens.

    Was derived as thinking_budget + 4096, a formula that only made sense while
    the budget was a number this stack chose. Adaptive thinking has no such
    number, so this is a flat value and the only thing standing between a model
    that starts looping and the budget's spend brake.

    Ten questions with four options and an explanation each runs to roughly 2k
    tokens of answer. The rest is headroom for thinking at the configured effort,
    plus the ~30% more tokens this model's tokenizer produces for the same text.

    Too low shows up as a truncated tool call rather than as anything naming the
    real cause, so the handler checks for it and says so explicitly.
  EOT
  type        = number
  default     = 16000

  validation {
    # The floor leaves room for the answer once thinking has taken its share; the
    # ceiling is this model's documented maximum output.
    condition     = var.bedrock_max_output_tokens >= 8192 && var.bedrock_max_output_tokens <= 128000
    error_message = "Max output tokens must be between 8192 and 128000."
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


# There is deliberately no `registration_token` variable. The enrolment
# secret is an SSM parameter referenced by name — see
# local.registration_token_parameter in lambda.tf — because a variable, even
# a sensitive one, lands in the function's environment and in state, and both
# are readable by the plan role from any pull request.

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
