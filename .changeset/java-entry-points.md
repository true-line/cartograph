---
"cartograph": minor
---

Java systems now show what starts their work: a `public static void main` (an HTTP server when it runs Spring Boot and the repository has Spring web endpoints), `@KafkaListener`/`@RabbitListener`/`@JmsListener`/`@SqsListener` methods as queue consumers with the topics or queues they name, `@Scheduled` methods as scheduled jobs with their schedule, and `CommandLineRunner`/`ApplicationRunner` beans as startup tasks. Each is an entry point flows start from and a capability in D1 §1.3. On the JHipster sample app: the Spring Boot server and the `removeNotActivatedUsers` scheduled job.
