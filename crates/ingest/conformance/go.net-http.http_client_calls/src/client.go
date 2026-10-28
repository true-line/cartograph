package client

import (
	"context"
	"net/http"
)

func sync(ctx context.Context) {
	http.Get("https://api.example.com/users")
	http.NewRequestWithContext(ctx, http.MethodDelete, "https://api.example.com/users/1", nil)
}
