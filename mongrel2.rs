export connect;
export connection;
export request;

type connection_t = {
    sender_id: option<str>,
    req_addrs: @~[str],
    rep_addrs: @~[str],
    req: zmq::socket,
    rep: zmq::socket,
};

fn connect(ctx: zmq::context,
           +sender_id: option<str>,
           +req_addrs: ~[str],
           +rep_addrs: ~[str]) -> connection {
    let req = alt ctx.socket(zmq::PULL) {
      ok(req) { req }
      err(e) { fail e.to_str() }
    };

    do req_addrs.iter |req_addr| {
        alt req.connect(req_addr) {
          ok(()) { }
          err(e) { fail e.to_str() }
        }
    }

    let rep = alt ctx.socket(zmq::PUB) {
      err(e) { fail e.to_str() }
      ok(rep) { rep }
    };

    alt sender_id {
      none { }
      some(sender_id) {
        alt rep.set_identity(sender_id) {
          ok(()) { }
          err(e) { fail e.to_str() }
        }
      }
    }

    for rep_addrs.each |rep_addr| {
        alt rep.connect(rep_addr) {
          ok(()) { }
          err(e) { fail e.to_str() }
        }
    }

    {
        sender_id: sender_id,
        req_addrs: @req_addrs,
        rep_addrs: @rep_addrs,
        req: req,
        rep: rep
    } as connection
}

type header_map = hashmap<str, @dvec<@str>>;

iface connection {
    fn req_addrs() -> @~[str];
    fn rep_addrs() -> @~[str];
    fn recv() -> result<@request, @str>;
    fn send(uuid: @str,
            id: &[const str],
            body: &[const u8]) -> result<(), @str>;
    fn reply(req: @request, body: &[const u8]) -> result<(), @str>;
    fn reply_http(req: @request,
                  code: uint,
                  status: str,
                  headers: header_map,
                  body: ~[u8]) -> result<(), @str>;
    fn term();
}

impl of connection for connection_t {
    fn req_addrs() -> @~[str] { self.req_addrs }
    fn rep_addrs() -> @~[str] { self.rep_addrs }

    fn recv() -> result<@request, @str> {
        alt self.req.recv(0) {
          err(e) { err(@e.to_str()) }
          ok(msg) { parse(msg) }
        }
    }

    fn send(uuid: @str,
            id: &[const str],
            body: &[const u8]) -> result<(), @str> {
        let id = str::bytes(str::connect(id, " "));
        let msg = dvec();
        do str::as_bytes(*uuid) |uuid| {
            msg.push_slice(uuid, 0u, uuid.len() - 1u);
        }
        msg.push(' ' as u8);
        msg.push_all(tnetstring::to_bytes(tnetstring::str(@id)));
        msg.push(' ' as u8);
        msg.push_slice(body, 0u, body.len());

        let msg = dvec::unwrap(msg);

        alt self.rep.send(msg, 0) {
          err(e) { err(@e.to_str()) }
          ok(()) { ok(()) }
        }
    }

    fn reply(req: @request, body: &[const u8]) -> result<(), @str> {
        self.send(req.uuid, ~[copy *req.id], body)
    }

    fn reply_http(req: @request,
                  code: uint,
                  status: str,
                  headers: header_map,
                  body: ~[u8]) -> result<(), @str> {
        let rep = dvec();
        rep.push_all(str::bytes(#fmt("HTTP/1.1 %u ", code)));
        rep.push_all(str::bytes(status));
        rep.push_all(str::bytes("\r\n"));
        rep.push_all(str::bytes("Content-Length: "));
        rep.push_all(str::bytes(uint::to_str(vec::len(body), 10u)));
        rep.push_all(str::bytes("\r\n"));

        for headers.each |key, values| {
            for (*values).each |value| {
                rep.push_all(str::bytes(key + ": " + *value + "\r\n"));
            };
        }
        rep.push_all(str::bytes("\r\n"));
        rep.push_all(body);

        self.reply(req, dvec::unwrap(rep))
    }

    fn term() {
        self.req.close();
        self.rep.close();
    }
}

type request = {
    uuid: @str,
    id: @str,
    path: @str,
    headers: header_map,
    body: @~[u8],
    json_body: option<hashmap<str, json::json>>,
};

impl request for @request {
    fn is_disconnect() -> bool {
        do self.json_body.map_default(false) |map| {
            alt map.find("type") {
              some(json::string(typ)) { *typ == "disconnect" }
              _ { false }
            }
        }
    }

    fn should_close() -> bool {
        alt self.headers.find("connection") {
          none { }
          some(conn) {
            if (*conn).len() == 1u && *(*conn)[0u] == "close" { ret true; }
          }
        }

        alt self.headers.find("VERSION") {
          none { false }
          some(version) {
            (*version).len() == 1u && *(*version)[0u] == "HTTP/1.0"
          }
        }
    }
}

fn parse(msg: ~[u8]) -> result<@request, @str> {
    let end = msg.len();

    let (start, uuid) = alt parse_uuid(msg, 0u, end) {
      ok((start, uuid)) { (start, uuid) }
      err(e) { ret err(e); }
    };

    let (start, id) = alt parse_id(msg, start, end) {
      ok(value) { value }
      err(e) { ret err(e); }
    };

    let (start, path) = alt parse_path(msg, start, end) {
      ok(value) { value }
      err(e) { ret err(e); }
    };

    let (headers, body) = alt parse_rest(msg, start, end) {
      ok(value) { value }
      err(e) { ret err(e); }
    };

    // Extract out the json body if we have it.
    let json_body = alt headers.find("METHOD") {
      none { none }
      some(method) {
        if method.len() == 1u && *method[0u] == "JSON" {
            alt json::from_str(str::from_bytes(copy *body)) {
              ok(json::dict(map)) { some(map) }
              ok(_) { ret err(@"json body is not a dictionary"); }
              err(e) { ret err(@#fmt("invalid JSON string: %s", e.to_str())); }
            }
        } else { none }
      }
    };

    ok(@{
        uuid: uuid,
        id: id,
        path: path,
        headers: headers,
        body: body,
        json_body: json_body
    })
}

fn parse_uuid(msg: ~[u8],
              start: uint,
              end: uint) -> result<(uint, @str), @str> {
    alt vec::position_between(msg, start, end, |c| c == ' ' as u8) {
        none { err(@"invalid sender uuid") }
        some(i) { ok((i + 1u, @str::from_bytes(vec::slice(msg, 0u, i)))) }
    }
}

fn parse_id(msg: ~[u8],
            start: uint,
            end: uint) -> result<(uint, @str), @str> {
    alt vec::position_between(msg, start, end, |c| c == ' ' as u8) {
      none { err(@"invalid connection id") }
      some(i) { ok((i + 1u, @str::from_bytes(vec::slice(msg, start, i)))) }
    }
}

fn parse_path(msg: ~[u8],
              start: uint,
              end: uint) -> result<(uint, @str), @str> {
    alt vec::position_between(msg, start, end, |c| c == ' ' as u8) {
      none { err(@"invalid path") }
      some(i) { ok((i + 1u, @str::from_bytes(vec::slice(msg, start, i)))) }
    }
}

fn parse_rest(msg: ~[u8],
              start: uint,
              end: uint) -> result<(header_map, @~[u8]), @str> {
    let rest = vec::slice(msg, start, end);

    let (headers, rest) = tnetstring::from_bytes(rest);
    let headers = alt headers {
      none { ret err(@"empty headers"); }
      some(headers) {
        alt parse_headers(headers) {
          err(e) { ret err(e); }
          ok(headers) { headers }
        }
      }
    };

    let (body, _) = tnetstring::from_bytes(rest);
    let body = alt body {
      none { ret err(@"empty body"); }
      some(body) {
        alt parse_body(body) {
          err(e) { ret err(e); }
          ok(body) { body }
        }
      }
    };

    ok((headers, body))
}

fn parse_headers(tns: tnetstring) -> result<header_map, @str> {
    let headers: header_map = map::str_hash();

    alt tns {
      tnetstring::map(map) {
        for map.each |key, value| {
            let key = str::from_bytes(copy key);
            let values = alt headers.find(key) {
              some(values) { values }
              none {
                let values = @dvec();
                headers.insert(key, values);
                values
              }
            };

            alt value {
              tnetstring::str(v) {
                (*values).push(@str::from_bytes(copy *v))
              }
              tnetstring::vec(vs) {
                for (*vs).each |v| {
                    alt v {
                      tnetstring::str(v) {
                        (*values).push(@str::from_bytes(copy *v))
                      }
                      _ { ret err(@"header value is not a string"); }
                    }
                }
              }
              _ { ret err(@"header value is not string"); }
            }
        }
      }

      // Fall back onto json if we got a string.
      tnetstring::str(bytes) {
        alt json::from_str(str::from_bytes(copy *bytes)) {
          err(e) { ret err(@"invalid JSON string"); }
          ok(json::dict(map)) {
            for map.each |key, value| {
                let values = alt headers.find(key) {
                  some(values) { values }
                  none {
                    let values = @dvec();
                    headers.insert(copy key, values);
                    values
                  }
                };

                alt value {
                  json::string(v) { (*values).push(v) }
                  json::list(vs) {
                    for (*vs).each |v| {
                      alt v {
                        json::string(v) { (*values).push(v) }
                        _ { ret err(@"header value is not a string"); }
                      }
                    }
                  }
                  _ { ret err(@"header value is not string"); }
                }
            }
          }
          ok(_) { ret err(@"header is not a dictionary"); }
        }
      }

      _ { ret err(@"invalid header"); }
    }

    ok(headers)
}

fn parse_body(tns: tnetstring) -> result<@~[u8], @str> {
    alt tns {
      tnetstring::str(body) { ok(body) }
      _ { err(@"invalid body") }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn test() {
        let ctx =
            alt zmq::init(1) {
              ok(ctx) { ctx }
              err(e) { fail e.to_str() }
            };

        let connection = connect(ctx,
            some("F0D32575-2ABB-4957-BC8B-12DAC8AFF13A"),
            ~["tcp://127.0.0.1:9998"],
            ~["tcp://127.0.0.1:9999"]);

        connection.term();
        ctx.term();
    }

    #[test]
    fn test_request_parse() {
        let request = parse(
            str::bytes("abCD-123 56 / 13:{\"foo\":\"bar\"},11:hello world,")
        ).get();

        assert *request.uuid == "abCD-123";
        assert *request.id == "56";
        assert request.headers.size() == 1u;
        *(*request.headers.get("foo"))[0u] == "bar";
        assert *request.body == str::bytes("hello world");
    }
}
