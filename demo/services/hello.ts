import { Service, Handler, Body, type ExtractContext } from "sky/decorators";
import {rateLimit} from "sky/runtime"

const greetExtract = { body: Body<{ name: string }>() } as const;

@Service({ lifetime: "singleton" })
class HelloService {
  @Handler({
    method: "POST",
    path: "/hello",
    extract: greetExtract,
    middleware: [rateLimit({bucket: "api", perMinute: 2e4, identifier: "ip"})]
  })
  async greet({ body }: ExtractContext<typeof greetExtract>) {
    return { message: `Hello ${body.name}` };
  }
}

export { HelloService };
