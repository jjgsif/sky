import {
  Service,
  Handler,
  Param,
  Query,
  ZodBody,
  type ExtractContext,
} from "sky/decorators";
import { Response, HttpError } from "sky/runtime";
import { z } from "zod";

// ── Zod schemas ─────────────────────────────────────────

const CreateUserSchema = z.object({
  name: z.string().min(1, "Name is required"),
  email: z.string().email("Invalid email format"),
  role: z.enum(["admin", "member"]).default("member"),
});

const UpdateUserSchema = z.object({
  name: z.string().min(1).optional(),
  email: z.string().email().optional(),
  role: z.enum(["admin", "member"]).optional(),
});

// ── Types ───────────────────────────────────────────────

interface User {
  id: string;
  name: string;
  email: string;
  role: string;
  createdAt: string;
}

// ── Extract descriptors ─────────────────────────────────

const createExtract = { body: ZodBody(CreateUserSchema) } as const;
const getExtract = { id: Param("id") } as const;
const listExtract = {
  page: Query("page"),
  limit: Query("limit"),
} as const;
const updateExtract = {
  id: Param("id"),
  body: ZodBody(UpdateUserSchema),
} as const;
const deleteExtract = { id: Param("id") } as const;

// ── Service ─────────────────────────────────────────────

@Service({ lifetime: "scoped" })
class UserService {
  /** In-memory store for the demo. */
  private static users = new Map<string, User>();
  private static nextId = 1;

  @Handler({
    method: "POST",
    path: "/users",
    extract: createExtract,
  })
  async createUser({ body }: ExtractContext<typeof createExtract>) {
    const id = String(UserService.nextId++);

    const user: User = {
      id,
      name: body.name,
      email: body.email,
      role: body.role,
      createdAt: new Date().toISOString(),
    };

    UserService.users.set(id, user);

    return new Response(user).status(201);
  }

  @Handler({
    method: "GET",
    path: "/users/:id",
    extract: getExtract,
  })
  async getUser({ id }: ExtractContext<typeof getExtract>) {
    const user = UserService.users.get(id);

    if (!user) {
      throw new HttpError(404, `User '${id}' not found`);
    }

    return user;
  }

  @Handler({
    method: "GET",
    path: "/users",
    extract: listExtract,
  })
  async listUsers({ page, limit }: ExtractContext<typeof listExtract>) {
    const pageNum = parseInt(page ?? "1", 10);
    const limitNum = parseInt(limit ?? "10", 10);

    const all = Array.from(UserService.users.values());
    const start = (pageNum - 1) * limitNum;
    const items = all.slice(start, start + limitNum);

    return {
      items,
      total: all.length,
      page: pageNum,
      limit: limitNum,
    };
  }

  @Handler({
    method: "PUT",
    path: "/users/:id",
    extract: updateExtract,
  })
  async updateUser({ id, body }: ExtractContext<typeof updateExtract>) {
    const user = UserService.users.get(id);

    if (!user) {
      throw new HttpError(404, `User '${id}' not found`);
    }

    if (body.name !== undefined) user.name = body.name;
    if (body.email !== undefined) user.email = body.email;
    if (body.role !== undefined) user.role = body.role;

    return user;
  }

  @Handler({
    method: "DELETE",
    path: "/users/:id",
    extract: deleteExtract,
  })
  async deleteUser({ id }: ExtractContext<typeof deleteExtract>) {
    const existed = UserService.users.delete(id);

    if (!existed) {
      throw new HttpError(404, `User '${id}' not found`);
    }

    return new Response(null).status(204);
  }
}

export { UserService };
