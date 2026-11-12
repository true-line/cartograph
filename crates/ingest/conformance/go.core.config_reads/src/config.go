package main

import "os"

func getenv(key string) string { return key }

func load(name string) (string, string) {
	return os.Getenv("DATABASE_URL"), os.Getenv(name)
}

func lookalike() string {
	return getenv("SHADOWED")
}
