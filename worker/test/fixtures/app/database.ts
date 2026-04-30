import { Service } from "@decorators";

@Service({ lifetime: "singleton" })
export class DatabaseClient {
    async query(sql: string): Promise<any> {
        return {};
    }
}