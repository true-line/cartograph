package io.demo;

import org.springframework.kafka.annotation.KafkaListener;
import org.springframework.kafka.core.KafkaTemplate;

public class Orders {
    private final KafkaTemplate<String, String> template;

    public Orders(KafkaTemplate<String, String> template) {
        this.template = template;
    }

    public void publish(String body) {
        template.send("orders", body);
    }

    @KafkaListener(topics = "orders", groupId = "billing")
    public void onOrder(String body) {
    }
}
