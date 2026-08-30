# ---------------------------------------------------------------------------
# Compute: two Rust functions on the OS-only runtime.
#
#   api       synchronous, reached from the browser through a Function URL,
#             routes internally on path. Authenticates every request itself.
#   generate  asynchronous, fired by S3 when a PDF lands. Reads the document,
#             asks Bedrock for questions, writes them to the table.
#
# They are split because their failure and cost profiles have nothing in common:
# api is a 10-second request/response path that must stay warm and cheap, while
# generate runs for minutes, holds a gigabyte, and spends money per token. One
# function sized for both would mean paying generate's memory on every keypress
# and giving the internet-facing handler permission to invoke Bedrock.
#
# provided.al2023 + arm64: there is no managed Rust runtime, and Graviton is
# ~20% cheaper per GB-second than x86 for identical work. Nothing in the
# toolchain is x86-only, so there is no reason to pay the difference.
# ---------------------------------------------------------------------------

locals {
  api_function_name      = "${var.project}-api"
  generate_function_name = "${var.project}-generate"

  # Referenced by name only, never read. The apply role's SSM permissions stop
  # at /${var.project}/config/*, so it could not read this parameter even if
  # asked to — which is the point. A `data "aws_ssm_parameter"` here would drag
  # the signing key into plaintext in the state file forever.
  jwt_signing_key_parameter = "/${var.project}/secret/jwt-signing-key"

  # Bedrock's inference-profile indirection needs permission on BOTH the profile
  # ARN and the underlying foundation-model ARN in every region the profile can
  # route to — hence the wildcard region, which is not laziness. Granting only
  # the profile produces an AccessDeniedException naming a model ARN you never
  # wrote down, which is a miserable thing to debug.
  #
  # Kept in step with bedrock_allowed_models in bootstrap/variables.tf. The
  # boundary caps these roles at the Nova Lite family regardless, so widening
  # here alone achieves nothing except misleading the next reader.
  bedrock_model_patterns = ["amazon.nova-lite-*", "amazon.nova-2-lite-*"]

  bedrock_model_arns = flatten([
    for pattern in local.bedrock_model_patterns : [
      "arn:${local.partition}:bedrock:*::foundation-model/${pattern}",
      "arn:${local.partition}:bedrock:*:${local.account_id}:inference-profile/*${pattern}",
    ]
  ])
}

# ---------------------------------------------------------------------------
# Placeholder package.
#
# Terraform does not build or ship the Rust binaries — a separate workflow
# cross-compiles them and calls UpdateFunctionCode. This zip exists only because
# CreateFunction refuses to run without a package, so it is the smallest legal
# one: a single file named `bootstrap`, which is the entrypoint name the
# provided.* runtimes look for.
#
# The `ignore_changes` block on both functions is what makes the split work. Any
# deploy changes source_code_hash out from under the state, and without the
# ignore every subsequent `tofu apply` would helpfully roll production back to
# this stub. Terraform owns the function's *configuration*; the workflow owns
# its *code*, and neither is allowed an opinion about the other.
#
# Output lands in .terraform/ so it is gitignored and disappears with a clean.
# ---------------------------------------------------------------------------

data "archive_file" "lambda_placeholder" {
  type        = "zip"
  output_path = "${path.module}/.terraform/lambda-placeholder.zip"

  source {
    filename = "bootstrap"
    content  = "#!/bin/sh\necho 'placeholder: real binary is shipped by the deploy workflow' >&2\nexit 1\n"
  }
}

# ---------------------------------------------------------------------------
# Log groups.
#
# Declared rather than left to Lambda. The group the service creates on first
# invocation has retention set to "Never Expire", which is a storage bill that
# only ever goes up and cannot be fixed retroactively without deleting the logs.
#
# The functions depend on these so the group exists before anything can invoke
# them; otherwise Lambda wins the race, creates the unbounded group itself, and
# the apply then fails on a name that already exists.
# ---------------------------------------------------------------------------

resource "aws_cloudwatch_log_group" "api" {
  name              = "/aws/lambda/${local.api_function_name}"
  retention_in_days = 14
}

resource "aws_cloudwatch_log_group" "generate" {
  name              = "/aws/lambda/${local.generate_function_name}"
  retention_in_days = 14
}

# ---------------------------------------------------------------------------
# Execution roles.
#
# One per function, sharing nothing. The api function is reachable by anyone on
# the internet who finds its URL; giving it bedrock:InvokeModel because the
# other function needs it would hand an unauthenticated endpoint a metered API.
# The blast radius of a handler bug is exactly the policy attached to it.
# ---------------------------------------------------------------------------

data "aws_iam_policy_document" "lambda_assume_role" {
  statement {
    effect  = "Allow"
    actions = ["sts:AssumeRole"]

    principals {
      type        = "Service"
      identifiers = ["lambda.amazonaws.com"]
    }
  }
}

# --- api -------------------------------------------------------------------

resource "aws_iam_role" "api" {
  name                 = "${local.api_function_name}-execution"
  description          = "Execution role for the ${local.api_function_name} function."
  assume_role_policy   = data.aws_iam_policy_document.lambda_assume_role.json
  permissions_boundary = local.permissions_boundary_arn
}

data "aws_iam_policy_document" "api" {
  statement {
    sid    = "ApplicationTable"
    effect = "Allow"
    actions = [
      "dynamodb:GetItem",
      "dynamodb:BatchGetItem",
      "dynamodb:Query",
      "dynamodb:PutItem",
      "dynamodb:UpdateItem",
      "dynamodb:DeleteItem",
    ]
    resources = [aws_dynamodb_table.app.arn]
  }

  # Presigned URLs are signed locally with these credentials and evaluated
  # against this role when the browser uses them, so the scope of the prefix
  # here is the scope of what a presigned URL can ever reach. Both verbs are
  # needed: PUT for the upload, GET for handing the PDF back to the reader.
  statement {
    sid       = "PresignDocumentObjects"
    effect    = "Allow"
    actions   = ["s3:PutObject", "s3:GetObject"]
    resources = ["${aws_s3_bucket.docs.arn}/docs/*"]
  }

  statement {
    sid       = "ReadJwtSigningKey"
    effect    = "Allow"
    actions   = ["ssm:GetParameter"]
    resources = ["arn:${local.partition}:ssm:${var.aws_region}:${local.account_id}:parameter${local.jwt_signing_key_parameter}"]
  }

  # A SecureString read is refused without Decrypt on the key behind it.
  # ViaService pins that to SSM, so a handler bug cannot turn this into a
  # general-purpose decryption oracle for anything else in the account.
  statement {
    sid       = "DecryptSigningKeyViaSSM"
    effect    = "Allow"
    actions   = ["kms:Decrypt"]
    resources = ["*"]

    condition {
      test     = "StringEquals"
      variable = "kms:ViaService"
      values   = ["ssm.${var.aws_region}.amazonaws.com"]
    }
  }

  # No logs:CreateLogGroup — the group is Terraform's, and withholding the
  # permission means a rename that misses this file fails loudly at runtime
  # instead of quietly recreating an unbounded-retention group.
  statement {
    sid       = "Logs"
    effect    = "Allow"
    actions   = ["logs:CreateLogStream", "logs:PutLogEvents"]
    resources = ["${aws_cloudwatch_log_group.api.arn}:*"]
  }
}

resource "aws_iam_role_policy" "api" {
  name   = "permissions"
  role   = aws_iam_role.api.id
  policy = data.aws_iam_policy_document.api.json
}

# --- generate --------------------------------------------------------------

resource "aws_iam_role" "generate" {
  name                 = "${local.generate_function_name}-execution"
  description          = "Execution role for the ${local.generate_function_name} function."
  assume_role_policy   = data.aws_iam_policy_document.lambda_assume_role.json
  permissions_boundary = local.permissions_boundary_arn
}

data "aws_iam_policy_document" "generate" {
  statement {
    sid    = "ApplicationTable"
    effect = "Allow"
    actions = [
      "dynamodb:GetItem",
      "dynamodb:BatchGetItem",
      "dynamodb:Query",
      "dynamodb:PutItem",
      "dynamodb:UpdateItem",
      "dynamodb:DeleteItem",
    ]
    resources = [aws_dynamodb_table.app.arn]
  }

  # Read-only, and only under docs/. This function is triggered by object
  # creation; it has no reason to be able to write back into the bucket that
  # triggers it, which is also how it avoids being able to retrigger itself.
  statement {
    sid       = "ReadUploadedDocuments"
    effect    = "Allow"
    actions   = ["s3:GetObject"]
    resources = ["${aws_s3_bucket.docs.arn}/docs/*"]
  }

  statement {
    sid    = "InvokeApprovedModels"
    effect = "Allow"
    actions = [
      "bedrock:InvokeModel",
      "bedrock:InvokeModelWithResponseStream",
      "bedrock:Converse",
      "bedrock:ConverseStream",
    ]
    resources = local.bedrock_model_arns
  }

  statement {
    sid       = "Logs"
    effect    = "Allow"
    actions   = ["logs:CreateLogStream", "logs:PutLogEvents"]
    resources = ["${aws_cloudwatch_log_group.generate.arn}:*"]
  }
}

resource "aws_iam_role_policy" "generate" {
  name   = "permissions"
  role   = aws_iam_role.generate.id
  policy = data.aws_iam_policy_document.generate.json
}

# ---------------------------------------------------------------------------
# Functions.
# ---------------------------------------------------------------------------

resource "aws_lambda_function" "api" {
  function_name = local.api_function_name
  role          = aws_iam_role.api.arn

  runtime       = "provided.al2023"
  architectures = ["arm64"]

  # Ignored by the custom runtime, but CreateFunction rejects a zip package
  # without one. Named for the file the runtime actually execs.
  handler = "bootstrap"

  filename         = data.archive_file.lambda_placeholder.output_path
  source_code_hash = data.archive_file.lambda_placeholder.output_base64sha256

  # 256MB is about the floor at which a Rust binary's cold start stops being
  # dominated by the CPU share that comes with the memory allocation — Lambda
  # scales both together, so the cheapest setting is rarely the cheapest run.
  memory_size = 256

  # Comfortably above anything this handler does. It is a bound on a stuck
  # request, not a budget: the Function URL's own 60s cap would otherwise be the
  # only limit, and 60s of a hung handler is 60s of billing per caller.
  timeout = 10

  # NO reserved_concurrent_executions, and not by oversight. The Function URL
  # below is unauthenticated, so bounding concurrency is exactly the control
  # this wants — but this account's total limit is 10, and PutFunctionConcurrency
  # refuses any reservation that leaves fewer than 10 unreserved. Every possible
  # value fails the apply. The account limit is therefore the concurrency bound
  # itself, which is tighter than anything worth reserving; revisit this if the
  # limit is ever raised, because at 1000 the exposure is real.

  environment {
    variables = {
      TABLE_NAME  = aws_dynamodb_table.app.name
      DOCS_BUCKET = aws_s3_bucket.docs.id

      # RP_ID is the apex while ORIGIN is the app subdomain, and they are
      # deliberately different — see the webauthn_rp_id variable. The handler
      # must check both: the RP ID scopes which credentials the browser will
      # offer, the origin is what actually pins an assertion to this site.
      RP_ID  = var.webauthn_rp_id
      ORIGIN = "https://${var.app_domain}"

      # Name, not value. Resolved at cold start by the handler so the key never
      # appears in the function configuration, in state, or in the console.
      JWT_SIGNING_KEY_PARAMETER = local.jwt_signing_key_parameter

      # The api signs the presigned PUT, so the size bound has to be enforced
      # here — generate only ever sees a file that already exists.
      MAX_UPLOAD_BYTES = tostring(var.max_upload_bytes)

      WEBAUTHN_CREDENTIALS = var.webauthn_credentials
    }
  }

  # See the placeholder block above: the deploy workflow owns the code, this
  # file owns everything else. Without this the two fight on every apply.
  lifecycle {
    ignore_changes = [filename, source_code_hash, s3_key, s3_object_version]
  }

  depends_on = [aws_cloudwatch_log_group.api]
}

# ---------------------------------------------------------------------------
# Function URL, not API Gateway.
#
# HTTP API would add a per-request charge and a second place for routing to
# live, to do nothing this application needs — no usage plans, no authorizers
# (the JWT check is inside the handler because the WebAuthn state lives there
# anyway), no custom domain. Function URLs are free.
#
# authorization_type NONE means AWS performs no check at all: every request
# reaches the handler and is billed, whether or not it carries a valid JWT. The
# handler must therefore authenticate before it touches the table, and the
# account's concurrency limit is doing the work reserved concurrency cannot —
# see the note on the function above.
#
# CORS is required, not decorative. The SPA is served from CloudFront at
# var.app_domain and calls this URL cross-origin, so without an allowed origin
# the browser blocks every response. Credentials stay off: the JWT travels in
# the Authorization header, so no cookie ever needs to cross origins.
# ---------------------------------------------------------------------------

resource "aws_lambda_function_url" "api" {
  function_name      = aws_lambda_function.api.function_name
  authorization_type = "NONE"

  cors {
    allow_origins     = ["https://${var.app_domain}"]
    allow_methods     = ["GET", "POST", "PUT", "DELETE"]
    allow_headers     = ["content-type", "authorization"]
    allow_credentials = false
    max_age           = 3600
  }
}

resource "aws_lambda_function" "generate" {
  function_name = local.generate_function_name
  role          = aws_iam_role.generate.arn

  runtime       = "provided.al2023"
  architectures = ["arm64"]
  handler       = "bootstrap"

  filename         = data.archive_file.lambda_placeholder.output_path
  source_code_hash = data.archive_file.lambda_placeholder.output_base64sha256

  # PDF text extraction holds the whole document in memory, and memory buys CPU
  # here as well — at 256MB the extraction alone would outlast the timeout.
  memory_size = 1024

  # Generation observed at 30-90s. The 300s ceiling exists so a model that
  # stalls or a pathological document fails and gets logged rather than being
  # cut off mid-write, leaving a document row with half a question set.
  timeout = 300

  # Asynchronous invocations are retried twice on failure, so one bad upload is
  # already up to three model calls. Reserved concurrency would be the right
  # bound on a burst of uploads and is unavailable — see the note on the api
  # function. DAILY_DOCUMENT_CAP is what actually holds the line here.

  environment {
    variables = {
      TABLE_NAME  = aws_dynamodb_table.app.name
      DOCS_BUCKET = aws_s3_bucket.docs.id

      MODEL_ID           = var.bedrock_model_id
      MAX_PAGES          = tostring(var.max_pages)
      MAX_DOCUMENT_BYTES = tostring(var.max_upload_bytes)
      DAILY_DOCUMENT_CAP = tostring(var.daily_document_cap)
    }
  }

  lifecycle {
    ignore_changes = [filename, source_code_hash, s3_key, s3_object_version]
  }

  depends_on = [aws_cloudwatch_log_group.generate]
}

# ---------------------------------------------------------------------------
# Upload trigger.
# ---------------------------------------------------------------------------

# source_account as well as source_arn: bucket names are guessable and an ARN
# alone carries no account, so without it a bucket of the same name in someone
# else's account could be pointed at this function.
resource "aws_lambda_permission" "generate_from_s3" {
  statement_id   = "AllowExecutionFromDocsBucket"
  action         = "lambda:InvokeFunction"
  function_name  = aws_lambda_function.generate.function_name
  principal      = "s3.amazonaws.com"
  source_arn     = aws_s3_bucket.docs.arn
  source_account = local.account_id
}

# This resource is authoritative for the whole bucket — S3 has one notification
# configuration, not a list, so anything added here later must be added to this
# block or it silently replaces what is here.
#
# The suffix filter is a cost control as much as a correctness one: without it
# every object written under docs/ starts a 1GB function.
resource "aws_s3_bucket_notification" "docs" {
  bucket = aws_s3_bucket.docs.id

  lambda_function {
    lambda_function_arn = aws_lambda_function.generate.arn
    events              = ["s3:ObjectCreated:*"]
    filter_prefix       = "docs/"
    filter_suffix       = ".pdf"
  }

  # S3 validates that it can invoke the function while creating the
  # configuration, so the permission has to exist first or the apply fails with
  # an unhelpful "Unable to validate the following destination configurations".
  depends_on = [aws_lambda_permission.generate_from_s3]
}

output "api_function_url" {
  description = "Base URL the SPA calls. Baked into the front-end build, so a change here needs a redeploy of the site."
  value       = aws_lambda_function_url.api.function_url
}

output "api_function_name" {
  description = "Target for the deploy workflow's UpdateFunctionCode."
  value       = aws_lambda_function.api.function_name
}

output "generate_function_name" {
  description = "Target for the deploy workflow's UpdateFunctionCode."
  value       = aws_lambda_function.generate.function_name
}

# ---------------------------------------------------------------------------
# Failure path for `generate`.
#
# S3 invokes asynchronously: Lambda retries twice, then the event is gone. The
# handler writes status: failed on errors it can catch, but it cannot write
# anything if it is OOM-killed, times out, or panics before reaching that code —
# and then the document sits in "processing" forever with no signal anywhere
# except a log group nobody reads.
#
# The queue is not consumed by anything. That is deliberate: its job is to make
# the failure visible and inspectable ("is anything in the DLQ?"), not to retry.
# Retrying a PDF that already crashed the parser twice is unlikely to help.
# ---------------------------------------------------------------------------

resource "aws_sqs_queue" "generate_failures" {
  name = "${var.project}-generate-failures"

  # Long enough to notice on a weekly rhythm; this is a personal app, not an
  # on-call rotation.
  message_retention_seconds = 1209600 # 14 days

  sqs_managed_sse_enabled = true
}

data "aws_iam_policy_document" "generate_dlq" {
  statement {
    sid       = "SendFailedInvocations"
    effect    = "Allow"
    actions   = ["sqs:SendMessage"]
    resources = [aws_sqs_queue.generate_failures.arn]
  }
}

resource "aws_iam_role_policy" "generate_dlq" {
  name   = "dlq"
  role   = aws_iam_role.generate.id
  policy = data.aws_iam_policy_document.generate_dlq.json
}

resource "aws_lambda_function_event_invoke_config" "generate" {
  function_name = aws_lambda_function.generate.function_name

  # Two attempts total. A malformed PDF fails identically every time, and each
  # retry is another Bedrock call against a $10 budget.
  maximum_retry_attempts = 1

  destination_config {
    on_failure {
      destination = aws_sqs_queue.generate_failures.arn
    }
  }
}

output "generate_failure_queue" {
  description = "Failed generate invocations land here. Nothing consumes it — check it when a document is stuck in 'processing'."
  value       = aws_sqs_queue.generate_failures.name
}
