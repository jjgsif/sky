import { Service, Handler, Group, Body, Header, Param, Query } from "@decorators";
import { DatabaseClient } from "./database";
import type { CreateUserInput, UserResponse, UpdateUserInput } from "./types";

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
        extract: [Body(), Header("x-tenant-id")],
    })
    async createUser(input: CreateUserInput, tenantId: string): Promise<UserResponse> {
        return {} as UserResponse;
    }

    @Handler({
        method: "GET",
        path: "/:id",
        extract: [Param("id")],
    })
    async getUser(id: string): Promise<UserResponse> {
        return {} as UserResponse;
    }

    @Handler({
        method: "GET",
        path: "/",
        extract: [Query("page"), Query("limit")],
    })
    async listUsers(page: number, limit: number): Promise<UserResponse[]> {
        return [];
    }

    @Handler({
        method: "PUT",
        path: "/:id",
        extract: [Param("id"), Body()],
    })
    async updateUser(id: string, input: UpdateUserInput): Promise<UserResponse> {
        return {} as UserResponse;
    }

    @Handler({
        method: "DELETE",
        path: "/:id",
        status: 204,
        validate: false,
        extract: [Param("id")],
    })
    async deleteUser(id: string): Promise<void> {}
}