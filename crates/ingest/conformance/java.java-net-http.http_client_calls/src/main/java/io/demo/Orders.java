package io.demo;

import java.net.URI;
import java.net.http.HttpClient;
import java.net.http.HttpRequest;
import java.net.http.HttpResponse;

class Orders {
    private final HttpClient client = HttpClient.newHttpClient();

    String list() throws Exception {
        HttpRequest request = HttpRequest.newBuilder(URI.create("https://api.example.com/orders"))
                .header("Accept", "application/json")
                .build();
        return client.send(request, HttpResponse.BodyHandlers.ofString()).body();
    }

    void create(String json) throws Exception {
        HttpRequest request = HttpRequest.newBuilder()
                .uri(URI.create("https://api.example.com/orders"))
                .POST(HttpRequest.BodyPublishers.ofString(json))
                .build();
        client.send(request, HttpResponse.BodyHandlers.discarding());
    }
}
