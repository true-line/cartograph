package io.demo

import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody

class Orders(private val client: OkHttpClient) {
    fun sync(body: RequestBody) {
        client.newCall(Request.Builder().url("https://api.example.com/orders").build()).execute()
        client.newCall(Request.Builder().url("https://api.example.com/orders").post(body).build()).execute()
    }
}
