import { Kafka } from "kafkajs";
const kafka = new Kafka({ brokers: [] });
const producer = kafka.producer();
export async function send() { await producer.send({ topic: "orders", messages: [] }); }
