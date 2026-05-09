/**
 * Service Registry
 *
 * Reads the ServiceMap populated by @sky/decorators and registers
 * each service with a @blue.ts/di Container. The container owns
 * the full dependency graph — resolution of any service triggers
 * its entire dependency chain.
 *
 * @sky/decorators uses TC39 Stage 3 decorators. The @Service class
 * decorator fires after all @Handler method decorators, capturing
 * the complete handler map, group, middleware, and dependency info
 * into a ServiceMap<Function, ServiceRegistration>.
 */

import { Container, type Identifier } from "@blue.ts/di";
import { ServiceMap } from "../decorators";
import type { ServiceRegistration, HandlerDefinition, ExtractDescriptor } from "../decorators";

// ── Types ───────────────────────────────────────────────

export interface RegisteredService {
  cls: new (...args: any[]) => any;
  name: string;
  registration: ServiceRegistration;
}

// ── Registry ────────────────────────────────────────────

export class ServiceRegistry {
  private services = new Map<string, RegisteredService>();
  public container: Container;

  constructor(container: Container) {
    this.container = container;
  }

  /**
   * Register a @Service-decorated class with the DI container.
   *
   * Reads the ServiceRegistration from ServiceMap (populated at
   * decoration time by @sky/decorators). Uses the registration's
   * lifetime and dependencies to configure the DI factory.
   */
  register(cls: new (...args: any[]) => any): void {
    const registration = ServiceMap.get(cls);

    if (!registration) {
      throw new Error(
        `Class ${cls.name} is not decorated with @Service — not found in ServiceMap`
      );
    }

    // Register with the DI container. The factory resolves
    // constructor dependencies through r.get(), triggering
    // the full dependency chain.
    this.container.register(cls, {
      lifetime: registration.lifetime,
      factory: async (r) => {
        const deps = await Promise.all(
          registration.dependencies.map((dep: string | Function | symbol) =>
            r.get(dep as Identifier<any>),
          )
        );
        return new cls(...deps);
      },
    });

    const name = registration.name ?? this.deriveServiceName(cls);

    this.services.set(name, {
      cls,
      name,
      registration,
    });
  }

  /**
   * Register multiple service classes at once.
   */
  registerAll(classes: (new (...args: any[]) => any)[]): void {
    for (const cls of classes) {
      this.register(cls);
    }
  }

  /**
   * Look up a registered service by name.
   */
  getServiceInfo(name: string): RegisteredService | undefined {
    return this.services.get(name);
  }

  /**
   * Look up a registered service by its original class name.
   * Used by the dispatcher to match generated Connect service types
   * (which use the class name) to registered services.
   */
  getServiceInfoByClassName(className: string): RegisteredService | undefined {
    for (const service of this.services.values()) {
      if (service.cls.name === className) {
        return service;
      }
    }
    return undefined;
  }

  /**
   * Get the class constructor for a service by name.
   */
  getServiceClass(name: string): (new (...args: any[]) => any) | undefined {
    return this.services.get(name)?.cls;
  }

  /**
   * Get handler definition for a specific method on a service.
   */
  getHandlerDefinition(
    serviceName: string,
    handlerName: string
  ): HandlerDefinition | undefined {
    return this.services.get(serviceName)?.registration.handlers.get(handlerName);
  }

  /**
   * Get the extract descriptors for a handler method, keyed by the field
   * name that will appear in the handler's input object.
   */
  getExtracts(
    serviceName: string,
    handlerName: string
  ): Record<string, ExtractDescriptor> {
    return this.getHandlerDefinition(serviceName, handlerName)?.extract ?? {};
  }

  /**
   * List all registered service names.
   */
  serviceNames(): string[] {
    return Array.from(this.services.keys());
  }

  /**
   * Derive the service name from the class.
   *
   * Convention: lowercase first letter of class name.
   * UserService → userService, HealthService → healthService.
   */
  private deriveServiceName(cls: new (...args: any[]) => any): string {
    const name = cls.name;
    return name.charAt(0).toLowerCase() + name.slice(1);
  }
}
