# ---------------------------------------------------------------------------
# Spend detection.
#
# The service allowlist and region lock are preventive controls, and both work
# service-by-service. They cannot stop denial-of-wallet that runs through an
# action which IS allowed — writing very large objects to an allowed S3 prefix,
# say. A spend-side control catches that class regardless of which API caused it.
#
# The account already had a console-created zero-spend budget, but console state
# is not code: nothing guaranteed it survived, and CI is denied budgets:* so it
# cannot be silently removed. This makes it reproducible.
#
# Budgets and Cost Explorer are global services addressed through us-east-1.
# Both of these are free.
# ---------------------------------------------------------------------------

provider "aws" {
  alias  = "us_east_1"
  region = "us-east-1"

  default_tags {
    tags = {
      Project   = var.project
      ManagedBy = "opentofu"
      Module    = "bootstrap"
    }
  }
}

resource "aws_budgets_budget" "monthly_cost" {
  provider = aws.us_east_1

  name         = "${var.project}-monthly-cost"
  budget_type  = "COST"
  limit_amount = var.monthly_budget_usd
  limit_unit   = "USD"
  time_unit    = "MONTHLY"

  # Thresholds are percentages so they track monthly_budget_usd rather than
  # needing to be re-tuned whenever the limit changes.
  #
  # 80% actual is the early warning; 100% actual is the limit itself; 100%
  # forecast is the one that matters most, because it fires on the day a runaway
  # resource starts rather than at month end when the money is already spent.
  notification {
    comparison_operator        = "GREATER_THAN"
    threshold                  = 80
    threshold_type             = "PERCENTAGE"
    notification_type          = "ACTUAL"
    subscriber_email_addresses = var.cost_alert_emails
  }

  notification {
    comparison_operator        = "GREATER_THAN"
    threshold                  = 100
    threshold_type             = "PERCENTAGE"
    notification_type          = "ACTUAL"
    subscriber_email_addresses = var.cost_alert_emails
  }

  notification {
    comparison_operator        = "GREATER_THAN"
    threshold                  = 100
    threshold_type             = "PERCENTAGE"
    notification_type          = "FORECASTED"
    subscriber_email_addresses = var.cost_alert_emails
  }
}

# Budgets evaluate a few times a day against a threshold. Anomaly detection is
# behavioural — it flags a sudden change in shape even when the absolute amount
# is still small, which on an account whose baseline is ~$0 is exactly the
# signal worth having.
#
# AWS permits exactly one DIMENSIONAL/SERVICE monitor per account and creates
# `Default-Services-Monitor` automatically, so this is imported rather than
# created — see scripts/bootstrap.sh. Bringing it under management is the point:
# as console-only state, nothing guaranteed it survived, and the audit flagged
# that the account's only spend detection existed outside code.
#
# The name is kept as AWS created it so the import is stable; renaming would
# force replacement and hit the same one-per-account limit.
resource "aws_ce_anomaly_monitor" "services" {
  provider = aws.us_east_1

  name              = "Default-Services-Monitor"
  monitor_type      = "DIMENSIONAL"
  monitor_dimension = "SERVICE"

  lifecycle {
    prevent_destroy = true
  }
}

#
# IMMEDIATE frequency only supports SNS subscribers — EMAIL is limited to DAILY
# and WEEKLY. For denial-of-wallet the delay is the whole point, so route
# through SNS and fan out to email from there rather than accepting a day's lag.
resource "aws_sns_topic" "cost_alerts" {
  provider = aws.us_east_1

  name = "${var.project}-cost-alerts"
}

data "aws_iam_policy_document" "cost_alerts_topic" {
  statement {
    sid    = "AllowCostAnomalyDetectionToPublish"
    effect = "Allow"

    principals {
      type        = "Service"
      identifiers = ["costalerts.amazonaws.com"]
    }

    actions   = ["SNS:Publish"]
    resources = [aws_sns_topic.cost_alerts.arn]

    # Confused-deputy protection. Without these, ANY account could point a Cost
    # Explorer anomaly subscription at this topic ARN — CE only checks that the
    # topic policy permits costalerts.amazonaws.com. A stranger's anomalies
    # would then deliver here, which is alert fatigue aimed at the one signal
    # that catches denial-of-wallet.
    #
    # The CloudTrail bucket policy in cloudtrail.tf got this right; this file
    # did not. Same pattern, applied consistently.
    condition {
      test     = "StringEquals"
      variable = "aws:SourceAccount"
      values   = [local.account_id]
    }

    condition {
      test     = "ArnLike"
      variable = "aws:SourceArn"
      values   = ["arn:${local.partition}:ce::${local.account_id}:anomalysubscription/*"]
    }
  }

  # Replacing the topic policy drops SNS's implicit owner statement. Restore it
  # explicitly, or future integrations fail with a confusing AccessDenied.
  statement {
    sid    = "AllowAccountOwnerFullControl"
    effect = "Allow"

    principals {
      type        = "AWS"
      identifiers = [local.account_id]
    }

    actions   = ["SNS:GetTopicAttributes", "SNS:SetTopicAttributes", "SNS:Subscribe", "SNS:Publish", "SNS:ListSubscriptionsByTopic"]
    resources = [aws_sns_topic.cost_alerts.arn]
  }
}

resource "aws_sns_topic_policy" "cost_alerts" {
  provider = aws.us_east_1

  arn    = aws_sns_topic.cost_alerts.arn
  policy = data.aws_iam_policy_document.cost_alerts_topic.json
}

# One subscription per address, deliberately more than one.
#
# A security alert channel should not depend on a mail-forwarding rule you
# cannot monitor. aws-root@marcusdunn.ca is a Cloudflare-forwarded alias: if the
# routing rule is missing, or the destination spam-filters it, the alert is lost
# silently and you find out from the bill. Subscribing the destination mailbox
# directly as well removes that single point of failure.
#
# AWS emails a confirmation link per address; alerts do not flow to an address
# until it is clicked. Unconfirmed subscriptions are visible via
# `aws sns get-topic-attributes` → SubscriptionsPending.
resource "aws_sns_topic_subscription" "cost_alerts_email" {
  provider = aws.us_east_1
  for_each = toset(var.cost_alert_emails)

  topic_arn = aws_sns_topic.cost_alerts.arn
  protocol  = "email"
  endpoint  = each.value
}

resource "aws_ce_anomaly_subscription" "immediate" {
  provider = aws.us_east_1

  name             = "${var.project}-anomaly-alerts"
  frequency        = "IMMEDIATE"
  monitor_arn_list = [aws_ce_anomaly_monitor.services.arn]

  subscriber {
    type    = "SNS"
    address = aws_sns_topic.cost_alerts.arn
  }

  depends_on = [aws_sns_topic_policy.cost_alerts]

  # IMMEDIATE frequency requires a threshold expression. $1 is deliberately low:
  # on an account that should cost pennies, a dollar of unexplained spend is
  # already the signal.
  threshold_expression {
    dimension {
      key           = "ANOMALY_TOTAL_IMPACT_ABSOLUTE"
      match_options = ["GREATER_THAN_OR_EQUAL"]
      values        = ["1"]
    }
  }
}
