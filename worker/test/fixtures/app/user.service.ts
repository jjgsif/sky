import {
    Service,
    Handler,
    Group,
    Body,
    Header,
    Param,
    Query,
    type ExtractContext,
} from "@sky/decorators";
import { DatabaseClient } from "./database";
import type { CreateUserInput, UserResponse, UpdateUserInput } from "./types";

const createExtract = {
    body: Body<CreateUserInput>(),
    tenantId: Header("x-tenant-id"),
} as const;

const getExtract = {
    id: Param("id"),
} as const;

const listExtract = {
    page: Query("page"),
    limit: Query("limit"),
} as const;

const updateExtract = {
    id: Param("id"),
    body: Body<UpdateUserInput>(),
} as const;

const deleteExtract = {
    id: Param("id"),
} as const;

@Service({
    lifetime: "scoped",
    dependencies: [DatabaseClient],
})
@Group({ prefix: "/api/v1/users" })
export class UserService {

    @Handler({
        method: "POST",
        path: "/",
        status: 201,
        extract: createExtract,
    })
    async createUser(
        _input: ExtractContext<typeof createExtract>,
    ): Promise<UserResponse> {
        return {} as UserResponse;
    }

    @Handler({
        method: "GET",
        path: "/:id",
        extract: getExtract,
    })
    async getUser(
        _input: ExtractContext<typeof getExtract>,
    ): Promise<UserResponse> {
        return {} as UserResponse;
    }

    @Handler({
        method: "GET",
        path: "/",
        extract: listExtract,
    })
    async listUsers(
        _input: ExtractContext<typeof listExtract>,
    ): Promise<UserResponse[]> {
        return [];
    }

    @Handler({
        method: "PUT",
        path: "/:id",
        extract: updateExtract,
    })
    async updateUser(
        _input: ExtractContext<typeof updateExtract>,
    ): Promise<UserResponse> {
        return {} as UserResponse;
    }

    @Handler({
        method: "DELETE",
        path: "/:id",
        status: 204,
        validate: false,
        extract: deleteExtract,
    })
    async deleteUser(
        _input: ExtractContext<typeof deleteExtract>,
    ): Promise<void> {}
}
