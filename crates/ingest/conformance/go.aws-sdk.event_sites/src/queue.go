package queue

import (
	"context"

	"github.com/aws/aws-sdk-go-v2/aws"
	"github.com/aws/aws-sdk-go-v2/service/sns"
	"github.com/aws/aws-sdk-go-v2/service/sqs"
)

func Enqueue(ctx context.Context, client *sqs.Client, body string) error {
	_, err := client.SendMessage(ctx, &sqs.SendMessageInput{
		QueueUrl:    aws.String("https://sqs.eu-west-1.amazonaws.com/1/orders"),
		MessageBody: aws.String(body),
	})
	return err
}

func Notify(ctx context.Context, client *sns.Client) error {
	_, err := client.Publish(ctx, &sns.PublishInput{
		TopicArn: aws.String("arn:aws:sns:eu-west-1:1:sent"),
		Message:  aws.String("m"),
	})
	return err
}
