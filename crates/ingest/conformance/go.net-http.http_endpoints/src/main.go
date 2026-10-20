package main
import "net/http"
func users(w http.ResponseWriter, r *http.Request) {}
func main() { http.HandleFunc("/users", users) }
