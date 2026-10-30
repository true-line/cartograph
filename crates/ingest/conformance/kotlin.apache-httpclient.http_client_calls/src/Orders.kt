package io.demo

import org.apache.hc.client5.http.classic.methods.HttpGet
import org.apache.hc.client5.http.classic.methods.HttpPost
import org.apache.hc.client5.http.impl.classic.CloseableHttpClient

class Orders(private val client: CloseableHttpClient) {
    fun sync() {
        client.execute(HttpGet("https://api.example.com/orders")) { it.code }
        client.execute(HttpPost("https://api.example.com/orders")) { it.code }
    }
}
