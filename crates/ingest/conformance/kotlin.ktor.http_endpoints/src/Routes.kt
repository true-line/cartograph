package demo

import io.ktor.server.application.Application
import io.ktor.server.routing.get
import io.ktor.server.routing.route
import io.ktor.server.routing.routing

fun Application.module() {
    routing {
        route("/users") {
            get("/{id}") { }
        }
    }
}
