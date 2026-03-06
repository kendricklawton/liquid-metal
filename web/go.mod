module github.com/kendricklawton/liquid-metal/web

go 1.25.0

require (
	connectrpc.com/connect v1.19.1
	github.com/a-h/templ v0.3.977
	github.com/go-chi/chi/v5 v5.2.1
	github.com/kendricklawton/liquid-metal/gen/go v0.0.0
	github.com/workos/workos-go/v6 v6.4.0
	github.com/yuin/goldmark v1.7.16
)

require (
	github.com/google/go-querystring v1.0.0 // indirect
	google.golang.org/protobuf v1.36.11 // indirect
)

replace github.com/kendricklawton/liquid-metal/gen/go => ../gen/go
