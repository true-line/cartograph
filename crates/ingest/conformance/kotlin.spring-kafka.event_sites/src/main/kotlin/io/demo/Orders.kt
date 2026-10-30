package io.demo

import org.springframework.kafka.annotation.KafkaListener
import org.springframework.kafka.core.KafkaTemplate

class Orders(private val kafka: KafkaTemplate<String, String>) {
    fun place(id: String) {
        kafka.send("orders", id)
    }

    @KafkaListener(topics = ["orders"], groupId = "billing")
    fun onOrder(message: String) {}
}
