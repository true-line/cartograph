package stream

import (
	"github.com/segmentio/kafka-go"
)

func Writer() *kafka.Writer {
	return &kafka.Writer{Addr: kafka.TCP("kafka:9092"), Topic: "orders"}
}

func Reader() *kafka.Reader {
	return kafka.NewReader(kafka.ReaderConfig{Brokers: []string{"kafka:9092"}, Topic: "orders", GroupID: "billing"})
}
