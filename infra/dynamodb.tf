# ---------------------------------------------------------------------------
# Single-table store: documents, attempts, and auth challenges.
#
#   PK              SK                    Contents
#   DOC#<id>        META                  title, status, s3 key, doc_tags[],
#                                         tag_version, questions[] + answer key
#   DOC#<id>        ATTEMPT#<iso8601>     responses[], doc_tags[], duration
#   REVIEW          <doc>#<qid>           spaced-review schedule
#
# WebAuthn ceremony rows (AUTH / CHALLENGE#..., AUTH / REGISTRATION#...) no
# longer live here — see the second table below for why.
#
# PROVISIONED, not on-demand. The perpetual free tier covers 25 RCU and 25 WCU
# but does NOT cover on-demand request charges, so on-demand would bill from the
# first request. 5/5 is a fifth of the allowance and ample for one reader.
#
# There is no IAM condition key for provisioned throughput, so nothing in the
# guardrails can stop a table being created with 40,000 WCU. The number here is
# the only thing preventing that, which is why it is explicit rather than left
# to a default.
# ---------------------------------------------------------------------------

resource "aws_dynamodb_table" "app" {
  name         = "${var.project}-reading-trainer"
  billing_mode = "PROVISIONED"

  read_capacity  = var.dynamodb_read_capacity
  write_capacity = var.dynamodb_write_capacity

  hash_key  = "pk"
  range_key = "sk"

  attribute {
    name = "pk"
    type = "S"
  }

  attribute {
    name = "sk"
    type = "S"
  }

  # Auth challenges are single-use and short-lived; TTL reaps the ones that are
  # issued and never redeemed, which is most of them.
  ttl {
    attribute_name = "expires_at"
    enabled        = true
  }

  # The reading history is the entire point of the application and cannot be
  # reconstructed. Backup storage is billed per GB — pennies at this size.
  point_in_time_recovery {
    enabled = true
  }

  # DeleteTable is refused by the service while this is set. The apply role
  # holds dynamodb:* and is now also denied DeleteTable and
  # UpdateContinuousBackups outright by bootstrap's app guardrails; this is
  # the table-side half of the same protection, and unlike prevent_destroy
  # below it binds the AWS CLI as well as OpenTofu.
  deletion_protection_enabled = true

  server_side_encryption {
    # AWS-owned key: free, and adequate given nothing here is a secret. A
    # customer-managed key would cost ~$1/month.
    enabled = false
  }

  lifecycle {
    prevent_destroy = true
  }
}

output "dynamodb_table_name" {
  description = "Application table."
  value       = aws_dynamodb_table.app.name
}

# ---------------------------------------------------------------------------
# Ceremony state: WebAuthn challenges and in-flight registrations.
#
#   PK      SK                    Contents
#   AUTH    CHALLENGE#<b64>       serialized PasskeyAuthentication, 60s TTL
#   AUTH    REGISTRATION#<uuid>   serialized PasskeyRegistration, 300s TTL
#
# A table of its own, not a partition of the one above, because of who writes
# here. POST /auth/challenge and POST /auth/verify are the only routes that
# need no session, and each call to either is a write — a put of a ~1 KB state
# row, or the DeleteItem that consumes one. In the application table those
# writes competed with the reader's own for 5 WCU, and the rows they left
# behind were read by every Scan behind the document list and history screens
# until the TTL sweep (documented as up to 48 hours) removed them. An hour at
# the gateway's rate limit could make the list unreadable for a day.
#
# Here the worst case is a slow login while an attack is running, and the
# per-route throttle in apigateway.tf makes even that unlikely.
#
# Same free tier: 1 RCU because nothing reads this table (consuming a row is
# DeleteItem, a write), 5 WCU because that covers both throttled routes at
# their limit with a row per call. Total provisioned across both tables is
# 6 RCU / 10 WCU against an allowance of 25 / 25.
#
# No point-in-time recovery: every row is worthless within minutes of being
# written, and enabling PITR is the one UpdateContinuousBackups call the app
# guardrails would refuse. No deletion protection for the same reason;
# DeleteTable is denied to CI regardless.
# ---------------------------------------------------------------------------

resource "aws_dynamodb_table" "auth" {
  name         = "${var.project}-reading-trainer-auth"
  billing_mode = "PROVISIONED"

  read_capacity  = 1
  write_capacity = 5

  hash_key  = "pk"
  range_key = "sk"

  attribute {
    name = "pk"
    type = "S"
  }

  attribute {
    name = "sk"
    type = "S"
  }

  # Best-effort reaping of rows that were issued and never redeemed, which is
  # most of them. The handler checks expires_at itself; this only bounds
  # storage.
  ttl {
    attribute_name = "expires_at"
    enabled        = true
  }

  server_side_encryption {
    enabled = false
  }
}

output "auth_table_name" {
  description = "Ceremony-state table for the api function."
  value       = aws_dynamodb_table.auth.name
}
