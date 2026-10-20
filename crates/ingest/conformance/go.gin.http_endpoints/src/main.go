package main
import "github.com/gin-gonic/gin"
func users(c *gin.Context) {}
func main() { r := gin.Default(); r.GET("/users", users) }
