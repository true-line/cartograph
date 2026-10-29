use rdkafka::consumer::{Consumer, StreamConsumer};
use rdkafka::producer::{FutureProducer, FutureRecord};

pub async fn place(producer: &FutureProducer, order: &str) {
    let _ = producer
        .send(FutureRecord::to("orders").key(order).payload(order), TIMEOUT)
        .await;
}

pub fn listen(consumer: &StreamConsumer) {
    consumer.subscribe(&["orders"]).expect("subscribe");
}
