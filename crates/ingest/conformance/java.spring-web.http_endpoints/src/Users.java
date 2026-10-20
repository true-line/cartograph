package demo;
import org.springframework.web.bind.annotation.*;
@RestController
class Users {
  @GetMapping("/users/{id}")
  String get(String id) { return ""; }
}
