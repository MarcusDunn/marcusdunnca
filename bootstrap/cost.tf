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

  # Alert well before the limit, then again when actually exceeded, then on a
  # forecast breach — the forecast notification is the one that catches a
  # runaway resource on the day it starts rather than at month end.
  notification {
    comparison_operator        = "GREATER_THAN"
    threshold                  = 1
    threshold_type             = "ABSOLUTE_VALUE"
    notification_type          = "ACTUAL"
    subscriber_email_addresses = [var.cost_alert_email]
  }

  notification {
    comparison_operator        = "GREATER_THAN"
    threshold                  = 100
    threshold_type             = "PERCENTAGE"
    notification_type          = "ACTUAL"
    subscriber_email_addresses = [var.cost_alert_email]
  }

  notification {
    comparison_operator        = "GREATER_THAN"
    threshold                  = 100
    threshold_type             = "PERCENTAGE"
    notification_type          = "FORECASTED"
    subscriber_email_addresses = [var.cost_alert_email]
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
  }
}

resource "aws_sns_topic_policy" "cost_alerts" {
  provider = aws.us_east_1

  arn    = aws_sns_topic.cost_alerts.arn
  policy = data.aws_iam_policy_document.cost_alerts_topic.json
}

# AWS emails a confirmation link; alerts do not flow until it is clicked.
resource "aws_sns_topic_subscription" "cost_alerts_email" {
  provider = aws.us_east_1

  topic_arn = aws_sns_topic.cost_alerts.arn
  protocol  = "email"
  endpoint  = var.cost_alert_email
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
