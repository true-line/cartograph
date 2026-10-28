package demo;
import jakarta.ws.rs.GET;
import jakarta.ws.rs.Path;
@Path("/users")
class Users {
  @GET
  @Path("/{id}")
  String get(String id) { return ""; }
}
