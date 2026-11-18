# #1844 fixture: two root modules declare the same address.
resource "aws_sqs_queue" "orders" {
  name = "orders-staging"
}

module "staging_dlq" {
  source = "../../modules/staging_dlq"
}
