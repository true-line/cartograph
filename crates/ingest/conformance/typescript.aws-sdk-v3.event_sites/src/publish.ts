import { SQSClient, SendMessageCommand } from "@aws-sdk/client-sqs";
const client = new SQSClient({});
export async function send() {
  await client.send(new SendMessageCommand({ QueueUrl: "orders", MessageBody: "x" }));
}
