import { Service, Handler, Body } from "@decorators";

@Service({ lifetime: "singleton" })
class HelloService {
  @Handler({
    method: "POST",
    path: "/hello",
    extract: [Body()],
  })
  async greet(body: { name: string }) {
    return { message: `Hello ${body.name}` };
  }
}

export { HelloService };