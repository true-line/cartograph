package io.demo

import org.springframework.web.reactive.function.client.WebClient

class Orders(private val client: WebClient) {
    fun sync(id: String) {
        client.get().uri("/orders/{id}", id).retrieve()
        WebClient.builder().baseUrl("https://orders.example.com").build().post().uri("/orders").retrieve()
    }
}
