import { Service, Handler, Body } from "@sky/decorators";
import { cors } from "@sky/runtime/middleware";

@Service({ lifetime: "singleton" })
class HelloService {
  @Handler({
    method: "POST",
    path: "/hello",
    extract: [Body()],
    middleware: [cors({ origins: ["*"] })],
  })
  async greet(body: { name: string }) {
    return { message: `Hello ${body.name}` };
  }
}

export { HelloService };