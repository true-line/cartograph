package demo;
import jakarta.ws.rs.GET;
import jakarta.ws.rs.Path;
@Path("/users")
public class Users {
  @GET
  @Path("/{id}")
  public String get(String id) { return ""; }
}
