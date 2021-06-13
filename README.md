## Memster in-memory cache
This is a in-memory cache with two endpoints:

* POST /set/<key> sets the given key associated to the payload
* GET /get/<key>  returns the associated the value to the given key or 404 if not present

Keys expire after 30 days but it can be configured from the `config.yaml`

### How to run
With docker:
`docker build -t memster . && docker run -p 8080:8080 memster`
Or 
`cargo run --release` 

## Design
The cache is built with concurrency in mind and the memory is thus sharded to reduce lock contention. The tricky part of that is the key distribution among the different shards and the distribution used by Memster is based on the implementation of the Dashmap crate which is a popular concurrent Hashmap and also use sharding internally. However the Dashmap crate can't be used directly since it has no concept of TTL. So a custom implementation is used instead which makes each shard keep track of the TTL of the keys. 

Much of the service can be configured via the `config.yaml`.

## Loadtest
A load test of the service that simulates the 80/20 read/write pattern described in the assignment can be found [here](https://github.com/Nehliin/memster-loadtest/tree/master).
