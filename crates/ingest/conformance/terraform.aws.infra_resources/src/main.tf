resource "aws_sqs_queue" "orders" { name = "orders" }
resource "aws_lambda_function" "worker" { function_name = "worker" }
resource "aws_lambda_event_source_mapping" "feed" {
  event_source_arn = aws_sqs_queue.orders.arn
  function_name    = aws_lambda_function.worker.arn
}
