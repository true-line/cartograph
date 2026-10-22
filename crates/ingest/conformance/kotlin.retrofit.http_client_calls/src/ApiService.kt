package io.demo

import retrofit2.Retrofit
import retrofit2.http.GET
import retrofit2.http.POST

interface ApiService {
    @GET("api/users/{id}")
    suspend fun user(id: Int): User

    @POST("api/users")
    suspend fun create(body: User): User
}

fun api(): ApiService = Retrofit.Builder()
    .baseUrl("https://api.example.com")
    .build()
    .create(ApiService::class.java)
