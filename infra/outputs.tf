output "account_id" {
  description = "AWS account ID."
  value       = local.account_id
}

output "access_analyzer_arn" {
  description = "ARN of the account-level IAM Access Analyzer."
  value       = aws_accessanalyzer_analyzer.account.arn
}
