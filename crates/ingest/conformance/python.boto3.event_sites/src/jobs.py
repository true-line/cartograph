import boto3

QUEUE = "https://sqs.eu-west-1.amazonaws.com/1/orders"
sqs = boto3.client("sqs")


def enqueue(body):
    sqs.send_message(QueueUrl=QUEUE, MessageBody=body)


def drain():
    return sqs.receive_message(QueueUrl=QUEUE)


def notify():
    boto3.client("sns").publish(TopicArn="arn:aws:sns:eu-west-1:1:sent", Message="m")
