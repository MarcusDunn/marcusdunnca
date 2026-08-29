output "account_id" {
  description = "AWS account ID this bootstrap was applied to."
  value       = local.account_id
}

output "region" {
  description = "Primary region."
  value       = var.aws_region
}

output "state_bucket" {
  description = "S3 bucket holding OpenTofu state for every root module in this repo."
  value       = aws_s3_bucket.state.id
}

output "plan_role_arn" {
  description = "Role assumed by pull_request jobs. Set as the GitHub repo variable AWS_PLAN_ROLE_ARN."
  value       = aws_iam_role.plan.arn
}

output "apply_role_arn" {
  description = "Role assumed by main-branch apply jobs. Set as the GitHub repo variable AWS_APPLY_ROLE_ARN."
  value       = aws_iam_role.apply.arn
}

output "ci_permissions_boundary_arn" {
  description = "Boundary every CI-created role must carry. Referenced by infra/ when it creates roles."
  value       = aws_iam_policy.ci_permissions_boundary.arn
}
