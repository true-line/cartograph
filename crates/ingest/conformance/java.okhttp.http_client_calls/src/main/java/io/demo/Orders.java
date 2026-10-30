package io.demo;

import okhttp3.MediaType;
import okhttp3.OkHttpClient;
import okhttp3.Request;
import okhttp3.RequestBody;
import okhttp3.Response;

class Orders {
    private static final MediaType JSON = MediaType.get("application/json");
    private final OkHttpClient client = new OkHttpClient();

    String list() throws Exception {
        Request request = new Request.Builder()
                .url("https://api.example.com/orders")
                .header("Accept", "application/json")
                .build();
        try (Response response = client.newCall(request).execute()) {
            return response.body().string();
        }
    }

    void create(String json) throws Exception {
        Request request = new Request.Builder()
                .url("https://api.example.com/orders")
                .post(RequestBody.create(json, JSON))
                .build();
        client.newCall(request).execute().close();
    }
}
