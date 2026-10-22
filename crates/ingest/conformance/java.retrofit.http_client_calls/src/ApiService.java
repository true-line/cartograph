package io.demo;

import retrofit2.Retrofit;
import retrofit2.http.GET;
import retrofit2.http.POST;

public interface ApiService {
    @GET("api/users/{id}")
    Call<User> user(@Path("id") int id);

    @POST("api/users")
    Call<User> create(@Body User body);

    static ApiService create() {
        return new Retrofit.Builder().baseUrl("https://api.example.com/").build().create(ApiService.class);
    }
}
