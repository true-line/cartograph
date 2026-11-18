# Child module: shares `aws_sqs_queue.dlq` with modules/prod_dlq, which is
# not a root-module collision.
resource "aws_sqs_queue" "dlq" {
  name = "orders-staging-dlq"
}
