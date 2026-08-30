# ---------------------------------------------------------------------------
# HTTP API in front of the api Lambda.
#
# This replaced a Lambda Function URL behind a CloudFront Origin Access
# Control. That arrangement worked but fought the design in two ways, both
# inherent rather than fixable:
#
#   * OAC signs every origin request with SigV4, and SigV4 *is* an
#     Authorization header — so CloudFront overwrote the viewer's bearer token
#     before the handler ever saw it. Login succeeded and the next request was
#     refused, with nothing in the logs.
#   * CloudFront cannot hash a body it is only forwarding, and Lambda rejects
#     unsigned payloads, so every POST required the browser to compute
#     x-amz-content-sha256 itself.
#
# An HTTP API needs neither. CloudFront talks to it as an ordinary HTTPS
# origin, headers pass through untouched, and bodies need no client-side
# hashing. This is the conventional shape for an authenticated API behind a
# distribution, and the reason it was not the first choice is cost — which at
# this traffic is a rounding error.
#
# ~$1.00 per million requests, first million free for twelve months. A
# single-reader app will not approach that.
# ---------------------------------------------------------------------------

resource "aws_apigatewayv2_api" "api" {
  name          = "${var.project}-api"
  protocol_type = "HTTP"
  description   = "Reading trainer api. Reached only through CloudFront at /api/*."

  # No CORS block. The SPA is served from the same origin as this API by the
  # distribution, so no preflight is ever issued. Configuring CORS in a second
  # place is what produced duplicate Access-Control-Allow-Origin headers when
  # both the Function URL and the handler set them.
}

resource "aws_apigatewayv2_integration" "api" {
  api_id           = aws_apigatewayv2_api.api.id
  integration_type = "AWS_PROXY"
  integration_uri  = aws_lambda_function.api.invoke_arn

  # 2.0 is what lambda_http expects for an HTTP API; 1.0 delivers a different
  # event shape and the handler would see no route match.
  payload_format_version = "2.0"

  # Comfortably above the handler's own 10s timeout, so a slow response fails
  # in the handler with a real error rather than being cut off here.
  timeout_milliseconds = 29000
}

# Proxy everything. Routing lives in the handler, which already dispatches on
# method and path — declaring routes here as well would mean two route tables
# to keep in step, and a mismatch would surface as a 404 from a component the
# handler cannot see.
resource "aws_apigatewayv2_route" "proxy" {
  api_id    = aws_apigatewayv2_api.api.id
  route_key = "$default"
  target    = "integrations/${aws_apigatewayv2_integration.api.id}"
}

resource "aws_cloudwatch_log_group" "apigateway" {
  name              = "/aws/apigateway/${var.project}-api"
  retention_in_days = 14
}

resource "aws_apigatewayv2_stage" "default" {
  api_id      = aws_apigatewayv2_api.api.id
  name        = "$default"
  auto_deploy = true

  # The reason this is worth having over the Function URL it replaced.
  #
  # Account Lambda concurrency is 10, shared between this function and
  # `generate`. Nothing previously bounded request rate: a burst against an
  # unauthenticated endpoint could exhaust it and stall document generation.
  # Throttling here rejects excess at the gateway, before any invocation.
  #
  # 10/sec sustained with a burst of 20 is far above what one reader generates
  # and far below what would starve the account.
  default_route_settings {
    throttling_rate_limit  = 10
    throttling_burst_limit = 20
  }

  access_log_settings {
    destination_arn = aws_cloudwatch_log_group.apigateway.arn
    # Deliberately no request body and no headers: the session token rides in a
    # header and the assertion body carries credential material. Route, status
    # and latency are what is actually useful when something 4xxs.
    format = jsonencode({
      requestId  = "$context.requestId"
      method     = "$context.httpMethod"
      path       = "$context.path"
      status     = "$context.status"
      latencyMs  = "$context.responseLatency"
      integError = "$context.integrationErrorMessage"
    })
  }
}

resource "aws_lambda_permission" "api_from_apigateway" {
  statement_id  = "AllowAPIGatewayInvoke"
  action        = "lambda:InvokeFunction"
  function_name = aws_lambda_function.api.function_name
  principal     = "apigateway.amazonaws.com"

  # Scoped to this API, so another account's gateway cannot point at the
  # function and inherit the permission.
  source_arn = "${aws_apigatewayv2_api.api.execution_arn}/*/*"
}

output "api_gateway_endpoint" {
  description = "Origin the distribution forwards /api/* to. Not called directly by clients."
  value       = aws_apigatewayv2_api.api.api_endpoint
}
