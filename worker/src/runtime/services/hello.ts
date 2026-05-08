import { Service, Handler, Body, type ExtractContext } from "@sky/decorators";
import { cors } from "@sky/runtime/middleware";

const greetExtract = { body: Body<{ name: string }>() } as const;

@Service({ lifetime: "singleton" })
class HelloService {
  @Handler({
    method: "POST",
    path: "/hello",
    extract: greetExtract,
    middleware: [cors({ origins: ["*"] })],
  })
  async greet({ body }: ExtractContext<typeof greetExtract>) {
    return { message: `Hello ${body.name}` };
  }
}

export { HelloService };