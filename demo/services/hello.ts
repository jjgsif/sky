import { Service, Handler, Body, type ExtractContext } from "sky-framework/decorators";

const greetExtract = { body: Body<{ name: string }>() } as const;

@Service({ lifetime: "singleton" })
class HelloService {
  @Handler({
    method: "POST",
    path: "/hello",
    extract: greetExtract,
  })
  async greet({ body }: ExtractContext<typeof greetExtract>) {
    return { message: `Hello ${body.name}` };
  }

  @Handler({
    method: "GET",
    path: "/hello/timeout-test",
    validate: false,
    timeout: 5_000,
  })
  async timeoutTest() {
    await new Promise(resolve => setTimeout(resolve, 10_000));
    return { message: "should never reach here" };
  }
}

export { HelloService };
