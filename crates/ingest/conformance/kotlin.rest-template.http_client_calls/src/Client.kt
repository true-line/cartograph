package io.demo

import org.springframework.web.client.RestTemplate

class Client(private val rest: RestTemplate) {
    fun user(id: Int): User =
        rest.getForObject("https://api.example.com/users/{id}", User::class.java, id)

    fun remove() = rest.delete("https://api.example.com/users/1")
}
