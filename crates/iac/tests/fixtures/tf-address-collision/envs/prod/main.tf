# #1844 fixture: two root modules declare the same address.
resource "aws_sqs_queue" "orders" {
  name = "orders-prod"
}

module "prod_dlq" {
  source = "../../modules/prod_dlq"
}
