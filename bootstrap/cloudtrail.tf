# ---------------------------------------------------------------------------
# CloudTrail — the account's audit record.
#
# The first management-events trail per account is free; only S3 storage costs
# anything, and management events on an idle account are a trickle. The apply
# role is explicitly denied StopLogging/DeleteTrail, so CI can evolve this trail
# but can never silence the record of its own actions.
# ---------------------------------------------------------------------------

locals {
  trail_name        = "${var.project}-management-events"
  trail_bucket_name = "${var.project}-cloudtrail-${local.account_id}"
  trail_arn         = "arn:${local.partition}:cloudtrail:${var.aws_region}:${local.account_id}:trail/${local.trail_name}"
}

resource "aws_s3_bucket" "trail" {
  bucket = local.trail_bucket_name

  lifecycle {
    prevent_destroy = true
  }
}

resource "aws_s3_bucket_versioning" "trail" {
  bucket = aws_s3_bucket.trail.id

  versioning_configuration {
    status = "Enabled"
  }
}

resource "aws_s3_bucket_server_side_encryption_configuration" "trail" {
  bucket = aws_s3_bucket.trail.id

  rule {
    apply_server_side_encryption_by_default {
      sse_algorithm = "AES256"
    }
  }
}

resource "aws_s3_bucket_public_access_block" "trail" {
  bucket = aws_s3_bucket.trail.id

  block_public_acls       = true
  block_public_policy     = true
  ignore_public_acls      = true
  restrict_public_buckets = true
}

resource "aws_s3_bucket_ownership_controls" "trail" {
  bucket = aws_s3_bucket.trail.id

  rule {
    object_ownership = "BucketOwnerEnforced"
  }
}

resource "aws_s3_bucket_lifecycle_configuration" "trail" {
  bucket = aws_s3_bucket.trail.id

  rule {
    id     = "expire-logs"
    status = "Enabled"

    filter {}

    expiration {
      days = var.cloudtrail_retention_days
    }

    noncurrent_version_expiration {
      noncurrent_days = 30
    }
  }

  depends_on = [aws_s3_bucket_versioning.trail]
}

data "aws_iam_policy_document" "trail_bucket" {
  statement {
    sid    = "AllowCloudTrailAclCheck"
    effect = "Allow"

    principals {
      type        = "Service"
      identifiers = ["cloudtrail.amazonaws.com"]
    }

    actions   = ["s3:GetBucketAcl"]
    resources = [aws_s3_bucket.trail.arn]

    # Confused-deputy protection: without this, any account's trail could be
    # pointed at this bucket.
    condition {
      test     = "StringEquals"
      variable = "aws:SourceArn"
      values   = [local.trail_arn]
    }
  }

  statement {
    sid    = "AllowCloudTrailWrite"
    effect = "Allow"

    principals {
      type        = "Service"
      identifiers = ["cloudtrail.amazonaws.com"]
    }

    actions   = ["s3:PutObject"]
    resources = ["${aws_s3_bucket.trail.arn}/AWSLogs/${local.account_id}/*"]

    condition {
      test     = "StringEquals"
      variable = "aws:SourceArn"
      values   = [local.trail_arn]
    }
  }

  statement {
    sid    = "DenyInsecureTransport"
    effect = "Deny"

    principals {
      type        = "*"
      identifiers = ["*"]
    }

    actions   = ["s3:*"]
    resources = [aws_s3_bucket.trail.arn, "${aws_s3_bucket.trail.arn}/*"]

    condition {
      test     = "Bool"
      variable = "aws:SecureTransport"
      values   = ["false"]
    }
  }

  # Mirrors the state bucket's policy. The audit log deserves at least the
  # protection the state file gets; the asymmetry here was an oversight.
  statement {
    sid    = "DenyOutdatedTLS"
    effect = "Deny"

    principals {
      type        = "*"
      identifiers = ["*"]
    }

    actions   = ["s3:*"]
    resources = [aws_s3_bucket.trail.arn, "${aws_s3_bucket.trail.arn}/*"]

    condition {
      test     = "NumericLessThan"
      variable = "s3:TlsVersion"
      values   = ["1.2"]
    }
  }

  # Restrict to this account's principals — but ONLY for IAM principals.
  #
  # The earlier version of this statement had just the aws:PrincipalAccount
  # condition, copied from the state bucket where it is harmless. On THIS bucket
  # it broke CloudTrail delivery for ~105 minutes: aws:PrincipalAccount is not
  # populated for AWS service principals, and StringNotEquals against a missing
  # key evaluates TRUE, so the deny matched cloudtrail.amazonaws.com's own
  # PutObject. The trail kept reporting IsLogging: true the whole time, with
  # LatestDeliveryError: AccessDenied.
  #
  # aws:PrincipalIsAWSService is false for IAM principals and true for service
  # principals, so requiring it to be false confines this deny to the case it
  # was written for. CloudTrail's writes are still constrained — by the
  # aws:SourceArn condition on AllowCloudTrailWrite above.
  statement {
    sid    = "DenyIAMPrincipalsOutsideThisAccount"
    effect = "Deny"

    principals {
      type        = "*"
      identifiers = ["*"]
    }

    actions   = ["s3:*"]
    resources = [aws_s3_bucket.trail.arn, "${aws_s3_bucket.trail.arn}/*"]

    condition {
      test     = "StringNotEquals"
      variable = "aws:PrincipalAccount"
      values   = [local.account_id]
    }

    condition {
      test     = "Bool"
      variable = "aws:PrincipalIsAWSService"
      values   = ["false"]
    }
  }
}

resource "aws_s3_bucket_policy" "trail" {
  bucket = aws_s3_bucket.trail.id
  policy = data.aws_iam_policy_document.trail_bucket.json

  depends_on = [aws_s3_bucket_public_access_block.trail]
}

resource "aws_cloudtrail" "management" {
  name           = local.trail_name
  s3_bucket_name = aws_s3_bucket.trail.id

  is_multi_region_trail         = true
  include_global_service_events = true
  enable_logging                = true

  # Lets you prove after the fact that no log file was altered or deleted.
  enable_log_file_validation = true

  # Management events alone leave the state bucket as an unrecorded read/write
  # channel: s3:GetObject/PutObject/DeleteObject are *data* events. Without this,
  # reading every state file, or tampering with state, produces no audit record
  # at all — which would undercut the "fully recorded" property the whole design
  # leans on.
  #
  # Scoped to the two managed buckets rather than all of S3, which is what keeps
  # this affordable: data events are billed per event, and these buckets see a
  # handful of operations per CI run.
  advanced_event_selector {
    name = "Management events"

    field_selector {
      field  = "eventCategory"
      equals = ["Management"]
    }
  }

  advanced_event_selector {
    name = "State and audit bucket object events"

    field_selector {
      field  = "eventCategory"
      equals = ["Data"]
    }

    field_selector {
      field  = "resources.type"
      equals = ["AWS::S3::Object"]
    }

    field_selector {
      field = "resources.ARN"
      starts_with = [
        "${aws_s3_bucket.state.arn}/",
        "${aws_s3_bucket.trail.arn}/",
      ]
    }
  }

  lifecycle {
    prevent_destroy = true
  }

  # CloudTrail validates it can write to the bucket at creation time.
  depends_on = [aws_s3_bucket_policy.trail]
}
