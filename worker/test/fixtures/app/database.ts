import { Service } from "@sky/decorators";

@Service({ lifetime: "singleton" })
export class DatabaseClient {
    async query(sql: string): Promise<any> {
        return {};
    }
}