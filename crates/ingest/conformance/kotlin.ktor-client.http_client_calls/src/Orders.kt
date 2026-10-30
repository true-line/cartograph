package io.demo

import io.ktor.client.HttpClient
import io.ktor.client.request.get
import io.ktor.client.request.post

class Orders(private val client: HttpClient) {
    suspend fun sync() {
        client.get("https://api.example.com/orders")
        client.post("https://api.example.com/orders")
    }
}
