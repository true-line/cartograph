package io.demo;

import org.springframework.cloud.openfeign.FeignClient;
import org.springframework.web.bind.annotation.GetMapping;
import org.springframework.web.bind.annotation.PathVariable;
import org.springframework.web.bind.annotation.PostMapping;

@FeignClient(name = "users", url = "https://users.example.com", path = "/api")
interface UsersClient {
    @GetMapping("/users/{id}")
    User get(@PathVariable("id") long id);

    @PostMapping("/users")
    User create(User user);
}
