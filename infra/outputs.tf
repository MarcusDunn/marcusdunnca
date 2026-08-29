output "account_id" {
  description = "AWS account ID."
  value       = local.account_id
}

output "cloudtrail_bucket" {
  description = "S3 bucket holding CloudTrail management-event logs."
  value       = aws_s3_bucket.trail.id
}

output "cloudtrail_arn" {
  description = "ARN of the account's management-events trail."
  value       = aws_cloudtrail.management.arn
}

output "access_analyzer_arn" {
  description = "ARN of the account-level IAM Access Analyzer."
  value       = aws_accessanalyzer_analyzer.account.arn
}
