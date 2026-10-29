from kafka import KafkaConsumer, KafkaProducer


def send(event):
    producer = KafkaProducer(bootstrap_servers="kafka:9092")
    producer.send("orders", event)


def listen():
    return KafkaConsumer("orders", group_id="billing")
