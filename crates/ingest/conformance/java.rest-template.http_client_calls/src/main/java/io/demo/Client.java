package io.demo;

import org.springframework.web.client.RestTemplate;

class Client {
    private final RestTemplate rest = new RestTemplate();

    String user() {
        return rest.getForObject("https://api.example.com/users/{id}", String.class, 1);
    }

    void remove() {
        rest.delete("https://api.example.com/users/1");
    }
}
