# ---------------------------------------------------------------------------
# Single-table store: documents, attempts, and auth challenges.
#
#   PK              SK                    Contents
#   DOC#<id>        META                  title, status, s3 key, doc_tags[],
#                                         tag_version, questions[] + answer key
#   DOC#<id>        ATTEMPT#<iso8601>     responses[], doc_tags[], duration
#   AUTH            CHALLENGE#<b64>       expires via TTL
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
