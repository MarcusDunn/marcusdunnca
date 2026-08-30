# ---------------------------------------------------------------------------
# Automated spend circuit breaker.
#
# Budgets and anomaly detection only send email — they tell you after the fact
# and rely on you being awake. Bedrock and CloudFront are both metered and
# neither is meaningfully bounded by the region lock, so an email is thin cover
# for the one class of failure that costs real money.
#
# A budget ACTION is different: at the threshold, AWS Budgets itself attaches a
# deny policy to the CI roles. No human, no Lambda, no dependency on anything in
# this repo still working.
#
# Free: the first two action-enabled budgets cost nothing, and there is one.
#
# Deliberately one-way. Detaching the brake requires a human with Identity
# Center admin — CI cannot modify its own role (DenyTamperingWithCIControlPlane),
# which is exactly the property wanted in a runaway-spend scenario.
# ---------------------------------------------------------------------------

data "aws_iam_policy_document" "spend_brake" {
  statement {
    sid       = "HaltMeteredServices"
    effect    = "Deny"
    actions   = ["bedrock:*"]
    resources = ["*"]
  }

  # Stops NEW distributions and config changes. It cannot stop an existing
  # distribution serving traffic — nothing in IAM can — but at 1 TB/month free
  # egress, CloudFront reaching this threshold means something is badly wrong
  # and freezing further change is the right move.
  statement {
    sid    = "HaltDistributionChanges"
    effect = "Deny"
    actions = [
      "cloudfront:CreateDistribution",
      "cloudfront:CreateDistributionWithTags",
      "cloudfront:UpdateDistribution",
    ]
    resources = ["*"]
  }

  statement {
    sid    = "HaltResourceCreation"
    effect = "Deny"
    actions = [
      "lambda:CreateFunction",
      "dynamodb:CreateTable",
      "dynamodb:UpdateTable",
      "s3:CreateBucket",
    ]
    resources = ["*"]
  }
}

resource "aws_iam_policy" "spend_brake" {
  name        = "${var.project}-spend-brake"
  description = "Attached automatically by AWS Budgets when spend crosses the threshold. Detach by hand after investigating."
  policy      = data.aws_iam_policy_document.spend_brake.json
}

# The role AWS Budgets assumes to apply the brake.
data "aws_iam_policy_document" "budgets_assume_role" {
  statement {
    effect  = "Allow"
    actions = ["sts:AssumeRole"]

    principals {
      type        = "Service"
      identifiers = ["budgets.amazonaws.com"]
    }

    # Confused-deputy protection: only this account's budgets may assume it.
    condition {
      test     = "StringEquals"
      variable = "aws:SourceAccount"
      values   = [local.account_id]
    }
  }
}

resource "aws_iam_role" "budgets_action" {
  name               = "${var.project}-budgets-action"
  description        = "Assumed by AWS Budgets to attach the spend brake to the CI roles."
  assume_role_policy = data.aws_iam_policy_document.budgets_assume_role.json
}

# Narrower than the AWS managed policy for this, which also grants SSM and EC2
# actions this account has no use for. Budgets only needs to attach and detach
# this one policy, on these two roles.
data "aws_iam_policy_document" "budgets_action" {
  statement {
    sid    = "AttachAndDetachTheBrake"
    effect = "Allow"
    actions = [
      "iam:AttachRolePolicy",
      "iam:DetachRolePolicy",
      "iam:ListAttachedRolePolicies",
      "iam:GetRole",
      "iam:GetPolicy",
    ]
    resources = [
      local.plan_role_arn,
      local.apply_role_arn,
      aws_iam_policy.spend_brake.arn,
    ]
  }
}

resource "aws_iam_role_policy" "budgets_action" {
  name   = "attach-spend-brake"
  role   = aws_iam_role.budgets_action.id
  policy = data.aws_iam_policy_document.budgets_action.json
}

resource "aws_budgets_budget_action" "spend_brake" {
  provider = aws.us_east_1

  budget_name        = aws_budgets_budget.monthly_cost.name
  action_type        = "APPLY_IAM_POLICY"
  approval_model     = "AUTOMATIC"
  notification_type  = "ACTUAL"
  execution_role_arn = aws_iam_role.budgets_action.arn

  action_threshold {
    action_threshold_type  = "PERCENTAGE"
    action_threshold_value = 90
  }

  definition {
    iam_action_definition {
      policy_arn = aws_iam_policy.spend_brake.arn
      roles      = [aws_iam_role.plan.name, aws_iam_role.apply.name]
    }
  }

  subscriber {
    address           = aws_sns_topic.cost_alerts.arn
    subscription_type = "SNS"
  }

  depends_on = [aws_iam_role_policy.budgets_action]
}
