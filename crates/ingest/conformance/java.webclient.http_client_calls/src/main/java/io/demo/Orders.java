package io.demo;

import org.springframework.web.reactive.function.client.WebClient;

class Orders {
    private final WebClient client = WebClient.create("https://orders.example.com");

    void sync(String id) {
        client.get().uri("/orders/{id}", id).retrieve();
        WebClient.builder().baseUrl("https://orders.example.com").build().post().uri("/orders").retrieve();
    }
}
